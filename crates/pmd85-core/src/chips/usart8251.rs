//! Minimal Intel 8251 USART emulation.
//!
//! The PMD 85 uses the 8251 for the cassette recorder interface and
//! V.24. This implementation models the programming interface (mode
//! word, command word, status register) enough for the Monitor ROM to
//! boot and to run the tape routines:
//!
//! - the DSR input (status bit 7) is driven externally by the tape
//!   deck (the Monitor 3 load routine bit-bangs the IRPS tape signal
//!   through it),
//! - the async receiver deserializes the RxD input on RxC rising
//!   edges (the PMD 85-1's original tape interface drives both lines
//!   from the deck; Monitor 1 reads tape bytes through the receiver),
//! - transmitted bytes are collected (the deck's recorder sniffs
//!   them), and
//! - TxRDY/TxEMPTY can be held low by the deck to pace the ROM's save
//!   routine at the authentic 1200-baud tape speed.
//!
//! Status register:
//! bit 0 TxRDY, bit 1 RxRDY, bit 2 TxEMPTY, bit 3 PE, bit 4 OE, bit 5 FE,
//! bit 6 SYNDET/BRKDET, bit 7 DSR.

/// Receiver state in async mode (a port of GPMD85's `ChipUSART8251`
/// receiver state machine, minus the sync-mode hunt).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RxState {
    /// Hunting for a start bit (RxD falling edge, edge-detected in
    /// `set_rxd`).
    WaitStart,
    /// Validating the start bit at the next RxC edge.
    StartBit,
    /// Shifting in data bits.
    DataBits,
    /// Parity bit (only with parity enabled).
    ParityBit,
    /// Stop bit: a low level is a framing error.
    StopBit,
}

#[derive(Clone, Debug)]
pub struct I8251 {
    mode_word: Option<u8>,
    command: u8,
    /// Internal status (errors, TxRDY/TxEMPTY); DSR, RxRDY and the
    /// transmit gate are combined in `read`.
    status: u8,
    /// Sync-character load phase after a sync mode word.
    sync_chars_pending: u8,
    /// Received data register; valid while RxRDY is set.
    pub rx_data: u8,
    /// Transmitted bytes (tape writes on a real machine).
    pub tx_log: Vec<u8>,
    /// Mirrors of output control lines.
    pub dtr: bool,
    pub rts: bool,
    /// External DSR line level (true = idle/mark).
    dsr: bool,
    /// Transmit-ready gate: `false` while the tape deck paces the
    /// transmitter (see `set_tx_ready`).
    tx_gate: bool,
    // ----- async receiver -----
    /// Receiver state machine position.
    rx_state: RxState,
    /// Deserialization shift register.
    rx_shift: u8,
    /// Data bits still expected (with parity, one more in
    /// `ParityBit`).
    rx_bits_left: u8,
    /// RxC edges left before the receiver samples (baud factor
    /// divider; 1 in async x1 mode).
    rx_factor_left: u8,
    /// Even parity accumulated over the data bits.
    rx_parity: bool,
    /// RxRDY: a character awaits in `rx_data`.
    rxrdy: bool,
    /// RxD / RxC line levels (true = idle/mark).
    rxd: bool,
    rxc: bool,
}

impl Default for I8251 {
    fn default() -> Self {
        Self::new()
    }
}

impl I8251 {
    /// Serialize the interface state into a save state. The DSR and
    /// transmit-gate lines are driven by the cassette deck and are not
    /// part of the snapshot: a restored machine normalizes them through
    /// the (always idle) deck.
    pub(crate) fn save_state(&self, w: &mut crate::state::StateWriter) {
        match self.mode_word {
            None => w.bool(false),
            Some(v) => {
                w.bool(true);
                w.u8(v);
            }
        }
        w.u8(self.command);
        w.u8(self.status);
        w.u8(self.sync_chars_pending);
        w.u8(self.rx_data);
        w.bool(self.dtr);
        w.bool(self.rts);
        w.len(self.tx_log.len());
        w.bytes(&self.tx_log);
    }

    /// Restore the interface state written by [`I8251::save_state`].
    pub(crate) fn load_state(
        &mut self,
        r: &mut crate::state::StateReader,
    ) -> Result<(), crate::state::StateError> {
        self.mode_word = r.bool()?.then(|| r.u8()).transpose()?;
        self.command = r.u8()?;
        self.status = r.u8()?;
        self.sync_chars_pending = r.u8()?;
        self.rx_data = r.u8()?;
        self.dtr = r.bool()?;
        self.rts = r.bool()?;
        self.tx_log = r.vec()?;
        Ok(())
    }
    pub fn new() -> Self {
        I8251 {
            mode_word: None,
            command: 0,
            status: Self::base_status(),
            sync_chars_pending: 0,
            rx_data: 0,
            tx_log: Vec::new(),
            dtr: false,
            rts: false,
            dsr: true,
            tx_gate: true,
            rx_state: RxState::WaitStart,
            rx_shift: 0,
            rx_bits_left: 0,
            rx_factor_left: 1,
            rx_parity: false,
            rxrdy: false,
            rxd: true,
            rxc: true,
        }
    }

    pub fn reset(&mut self) {
        *self = I8251::new();
    }

    fn base_status() -> u8 {
        // TxRDY | TxEMPTY (ready for the next byte; drains instantly
        // unless the tape deck gates it).
        0x05
    }

    /// Drive the external DSR line (status bit 7). The tape deck
    /// toggles this to feed the IRPS signal to the monitor.
    pub fn set_dsr(&mut self, level: bool) {
        self.dsr = level;
    }

    /// Gate TxRDY/TxEMPTY low while the tape deck simulates the byte
    /// shifting out at tape speed; released with `true`.
    pub fn set_tx_ready(&mut self, ready: bool) {
        self.tx_gate = ready;
    }

    /// Write to the control (1) or data (0) register.
    pub fn write(&mut self, reg: u32, data: u8) {
        match reg & 1 {
            0 => {
                // transmit data; the buffer drains instantly, the
                // pacing (if any) is applied externally
                self.tx_log.push(data);
                self.status = Self::base_status();
            }
            _ => self.write_control(data),
        }
    }

    fn write_control(&mut self, data: u8) {
        if self.mode_word.is_none() {
            // First write after reset is the mode word.
            self.mode_word = Some(data);
            // Sync mode (baud factor bits = 00) requires sync characters.
            if data & 0x03 == 0 {
                self.sync_chars_pending = if data & 0x10 != 0 { 2 } else { 1 };
            }
            // A new mode word resets the receiver.
            self.rx_reset();
            return;
        }
        if self.sync_chars_pending > 0 {
            self.sync_chars_pending -= 1;
            return;
        }
        // Otherwise this is a command word.
        self.command = data;
        // Internal reset (bit 6) returns to mode-word expectation.
        if data & 0x40 != 0 {
            self.mode_word = None;
            self.sync_chars_pending = 0;
            self.rx_reset();
        }
        self.dtr = data & 0x01 != 0;
        self.rts = data & 0x20 != 0;
        // Error reset (bit 4).
        if data & 0x10 != 0 {
            self.status &= !(0x38);
        }
    }

    /// Async mode is active (a mode word was programmed without the
    /// sync baud factor). Only then does the receiver run.
    fn async_mode(&self) -> bool {
        matches!(self.mode_word, Some(mw) if mw & 0x03 != 0)
    }

    /// Baud factor divider on RxC edges (async x1 / x16 / x64).
    fn factor(&self) -> u8 {
        match self.mode_word.map(|mw| mw & 0x03) {
            Some(1) => 1,
            Some(2) => 16,
            Some(3) => 64,
            _ => 1,
        }
    }

    /// Character length in bits (mode word bits 2-3, 5..=8).
    fn char_len(&self) -> u8 {
        match self.mode_word {
            Some(mw) => ((mw >> 2) & 3) + 5,
            None => 8,
        }
    }

    /// Parity enabled (mode word bit 4).
    fn parity_enabled(&self) -> bool {
        matches!(self.mode_word, Some(mw) if mw & 0x10 != 0)
    }

    /// Reset the receiver to start-bit hunting, dropping any
    /// pending character (the chip does this on a mode word write).
    fn rx_reset(&mut self) {
        self.rx_state = RxState::WaitStart;
        self.rxrdy = false;
        self.rx_factor_left = 1;
        self.rxd = true;
        self.rxc = true;
    }

    /// Drive the external RxD input. In async mode a falling edge
    /// while hunting for a start bit latches it for validation at
    /// the next RxC edge.
    pub fn set_rxd(&mut self, level: bool) {
        if self.async_mode() && self.rx_state == RxState::WaitStart && self.rxd && !level {
            self.rx_state = RxState::StartBit;
            let f = self.factor();
            self.rx_factor_left = if f > 1 { f / 2 } else { f };
        }
        self.rxd = level;
    }

    /// Drive the external RxC input; the receiver samples RxD at
    /// every rising edge (divided by the baud factor).
    pub fn set_rxc(&mut self, level: bool) {
        let rising = level && !self.rxc;
        self.rxc = level;
        if !rising || !self.async_mode() {
            return;
        }
        let f = self.factor();
        if f > 1 {
            self.rx_factor_left = self.rx_factor_left.saturating_sub(1);
            if self.rx_factor_left > 0 {
                return;
            }
            self.rx_factor_left = f;
        }
        match self.rx_state {
            RxState::WaitStart => {}
            RxState::StartBit => {
                if self.rxd {
                    // Not a start bit, just noise: back to hunting.
                    self.rx_state = RxState::WaitStart;
                } else {
                    self.rx_state = RxState::DataBits;
                    self.rx_shift = 0;
                    self.rx_bits_left = self.char_len();
                }
            }
            RxState::DataBits => {
                self.rx_shift >>= 1;
                if self.rxd {
                    self.rx_shift |= 0x80;
                }
                self.rx_bits_left -= 1;
                if self.rx_bits_left == 0 {
                    let len = self.char_len();
                    if len < 8 {
                        self.rx_shift >>= 8 - len;
                    }
                    if self.parity_enabled() {
                        self.rx_state = RxState::ParityBit;
                        self.rx_parity = self.rx_shift.count_ones() % 2 == 1;
                    } else {
                        self.rx_state = RxState::StopBit;
                    }
                }
            }
            RxState::ParityBit => {
                if self.rxd != self.rx_parity {
                    self.status |= 0x08; // PE
                }
                self.rx_state = RxState::StopBit;
            }
            RxState::StopBit => {
                if !self.rxd {
                    self.status |= 0x20; // FE: invalid stop bit
                }
                self.char_received();
                self.rx_state = RxState::WaitStart;
            }
        }
    }

    /// A character completed reception: land it in the data
    /// register and raise RxRDY (with overrun if the previous one
    /// was not read).
    fn char_received(&mut self) {
        self.rx_data = self.rx_shift;
        if self.rxrdy {
            self.status |= 0x10; // OE
        }
        self.rxrdy = true;
    }

    /// Whether a received character awaits in the data register.
    pub fn rx_ready(&self) -> bool {
        self.rxrdy
    }

    /// Feed a character straight into the receiver (the V1 tape
    /// interface's flash load pumps bytes this way instead of
    /// modulating the signal).
    pub fn feed_rx_byte(&mut self, val: u8) {
        self.rx_shift = val;
        self.char_received();
    }

    /// Read the status (1) or received data (0) register.
    pub fn read(&mut self, reg: u32) -> u8 {
        match reg & 1 {
            0 => {
                // Reading the data register picks up the character
                // and drops RxRDY.
                self.rxrdy = false;
                self.rx_data
            }
            _ => {
                let mut status = self.status;
                if self.rxrdy {
                    status |= 0x02;
                }
                if !self.tx_gate {
                    status &= !(0x05); // TxRDY | TxEMPTY
                }
                if self.dsr {
                    status |= 0x80;
                } else {
                    status &= !0x80;
                }
                status
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_then_command_sequence() {
        let mut uart = I8251::new();
        // async mode word: 8 data bits, x16, 1 stop
        uart.write(1, 0x4E);
        // command: enable tx/rts
        uart.write(1, 0x27);
        // status: TxRDY and TxEMPTY high
        assert_eq!(uart.read(1) & 0x07, 0x05);
        assert!(uart.rts);
    }

    #[test]
    fn internal_reset_returns_to_mode_word() {
        let mut uart = I8251::new();
        uart.write(1, 0x4E);
        uart.write(1, 0x40); // internal reset
                             // next control write is a mode word again
        uart.write(1, 0x40); // sync mode word
        uart.write(1, 0x16); // sync char
        uart.write(1, 0x27); // command
        assert_eq!(uart.read(1) & 0x05, 0x05);
    }

    #[test]
    fn tx_data_collected() {
        let mut uart = I8251::new();
        uart.write(1, 0x4E);
        uart.write(1, 0x27);
        uart.write(0, 0x55);
        uart.write(0, 0xAA);
        assert_eq!(uart.tx_log, vec![0x55, 0xAA]);
        // TxRDY remains set since the buffer drains instantly
        assert_eq!(uart.read(1) & 0x01, 0x01);
    }

    #[test]
    fn rx_never_ready() {
        let mut uart = I8251::new();
        assert_eq!(uart.read(1) & 0x02, 0, "RxRDY must stay low without tape");
    }

    /// Serialize one byte the way the V1 tape deck does: start bit,
    /// 8 data bits LSB first, stop bit, one bit per RxC period
    /// (rising edges), async x1 8N1.
    fn v1_send(uart: &mut I8251, byte: u8) {
        let bit = |uart: &mut I8251, level: bool| {
            uart.set_rxd(level);
            uart.set_rxc(false);
            uart.set_rxc(true);
        };
        bit(uart, false); // start
        for i in 0..8 {
            bit(uart, byte & (1 << i) != 0);
        }
        bit(uart, true); // stop
    }

    #[test]
    fn v1_receiver_deserializes_async_bytes() {
        let mut uart = I8251::new();
        // Monitor 1 programs async x1, 8 data bits, 2 stop bits.
        uart.write(1, 0xED);
        uart.write(1, 0x25); // RxEN | TxEN
        for byte in [0x00u8, 0x55, 0xA7, 0xFF] {
            v1_send(&mut uart, byte);
            assert!(uart.rx_ready(), "byte {byte:#04x} received");
            assert_eq!(uart.read(1) & 0x02, 0x02, "RxRDY in status");
            assert_eq!(uart.read(0), byte, "data register");
            assert!(!uart.rx_ready(), "read drops RxRDY");
        }
        // No framing or overrun errors accumulated.
        assert_eq!(uart.read(1) & 0x38, 0);
    }

    #[test]
    fn v1_receiver_ignores_idle_and_noise() {
        let mut uart = I8251::new();
        uart.write(1, 0xED);
        uart.write(1, 0x25);
        // Idle mark with the clock running must not produce data.
        for _ in 0..64 {
            uart.set_rxd(true);
            uart.set_rxc(false);
            uart.set_rxc(true);
        }
        assert!(!uart.rx_ready());
        // A lone low glitch latches a start-bit candidate, but the
        // validation sample sees the line back at mark: back to
        // hunting, no character.
        uart.set_rxd(false); // falling edge: candidate start bit
        uart.set_rxd(true); // glitch over before the first sample
        uart.set_rxc(false);
        uart.set_rxc(true); // sample: mark, not a start bit
        for _ in 0..16 {
            uart.set_rxd(true);
            uart.set_rxc(false);
            uart.set_rxc(true);
        }
        assert!(!uart.rx_ready(), "glitch produced no character");
    }

    #[test]
    fn v1_receiver_frame_error_on_missing_stop() {
        let mut uart = I8251::new();
        uart.write(1, 0xED);
        uart.write(1, 0x25);
        // Send a byte whose stop bit is low: framing error, but the
        // character still lands.
        uart.set_rxd(false);
        uart.set_rxc(false);
        uart.set_rxc(true);
        for i in 0..8 {
            uart.set_rxd(i == 0);
            uart.set_rxc(false);
            uart.set_rxc(true);
        }
        uart.set_rxd(false); // invalid stop
        uart.set_rxc(false);
        uart.set_rxc(true);
        assert!(uart.rx_ready());
        assert_eq!(uart.read(1) & 0x20, 0x20, "FE set");
    }

    #[test]
    fn v1_receiver_overrun_flags_oe() {
        let mut uart = I8251::new();
        uart.write(1, 0xED);
        uart.write(1, 0x25);
        uart.feed_rx_byte(0x11);
        assert!(uart.rx_ready());
        uart.feed_rx_byte(0x22); // previous not read: overrun
        assert_eq!(uart.read(1) & 0x10, 0x10, "OE set");
        assert_eq!(uart.read(0), 0x22, "latest byte kept");
    }

    #[test]
    fn v1_receiver_off_before_mode_word() {
        let mut uart = I8251::new();
        // No mode word: RxD/RxC traffic must not produce anything.
        uart.set_rxd(false);
        uart.set_rxc(false);
        uart.set_rxc(true);
        assert!(!uart.rx_ready());
        assert_eq!(uart.read(1) & 0x02, 0);
    }

    #[test]
    fn v1_receiver_reset_by_mode_word() {
        let mut uart = I8251::new();
        uart.write(1, 0xED);
        uart.write(1, 0x25);
        uart.feed_rx_byte(0x42);
        assert!(uart.rx_ready());
        uart.write(1, 0x40); // internal reset
        uart.write(1, 0xED); // reprogrammed mode word
        assert!(!uart.rx_ready(), "mode word resets the receiver");
    }

    #[test]
    fn dsr_line_reflects_in_status() {
        let mut uart = I8251::new();
        assert_eq!(uart.read(1) & 0x80, 0x80, "DSR idles high");
        uart.set_dsr(false);
        assert_eq!(uart.read(1) & 0x80, 0);
        uart.set_dsr(true);
        assert_eq!(uart.read(1) & 0x80, 0x80);
        // A data write must not disturb the external line state.
        uart.write(0, 0x42);
        assert_eq!(uart.read(1) & 0x80, 0x80);
    }

    #[test]
    fn tx_gate_holds_back_txrdy_and_txempty() {
        let mut uart = I8251::new();
        assert_eq!(uart.read(1) & 0x05, 0x05);
        uart.set_tx_ready(false);
        assert_eq!(uart.read(1) & 0x05, 0, "transmitter held busy");
        // Other bits are unaffected.
        assert_eq!(uart.read(1) & 0x80, 0x80);
        uart.set_tx_ready(true);
        assert_eq!(uart.read(1) & 0x05, 0x05);
    }

    #[test]
    fn reset_restores_external_lines() {
        let mut uart = I8251::new();
        uart.set_dsr(false);
        uart.set_tx_ready(false);
        uart.reset();
        assert_eq!(uart.read(1), 0x85);
    }
}
