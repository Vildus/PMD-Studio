//! Joystick 4004/482: the five-button stick wired to the I/O board's
//! GPIO connectors (K3 = GPIO/0, joystick 0 on port A `4Ch`; K4 =
//! GPIO/1, joystick 1 on port B `4Dh`).
//!
//! The stick sits behind the bidirectional MH8286 bus drivers: the
//! program initializes the 8255 with mode word `92h` (PA/PB mode 0
//! inputs, PC output) and writes `11h` to port `4Eh` — a 1 on PC4
//! (GPIO/0) resp. PC0 (GPIO/1) opens a transistor that sets the
//! driver direction to input and provides the ground for the lower
//! contacts of the buttons. A pressed button then pulls its
//! otherwise-high input line low, so the stick is read in negated
//! form: the program inverts with `CMA` and masks the five valid bits
//! with `ANI 1Fh`.
//!
//! Bit order from bit 0 (after inversion): down, up, right, left,
//! fire. Bits 5-7 are not wired and read as ones.

/// A button of the 4004/482 joystick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    Down,
    Up,
    Right,
    Left,
    Fire,
}

impl Button {
    /// The button's bit in the port read (cleared while pressed).
    pub fn bit(self) -> u8 {
        match self {
            Button::Down => 1 << 0,
            Button::Up => 1 << 1,
            Button::Right => 1 << 2,
            Button::Left => 1 << 3,
            Button::Fire => 1 << 4,
        }
    }

    /// All five buttons, in bit order.
    pub const ALL: [Button; 5] = [
        Button::Down,
        Button::Up,
        Button::Right,
        Button::Left,
        Button::Fire,
    ];
}

/// State of one joystick connector's five buttons.
#[derive(Clone, Debug, Default)]
pub struct Joystick {
    /// One bit per button ([`Button::bit`]); set = pressed.
    pressed: u8,
}

impl Joystick {
    pub fn new() -> Self {
        Joystick { pressed: 0 }
    }

    pub fn reset(&mut self) {
        self.pressed = 0;
    }

    pub fn set_button(&mut self, button: Button, pressed: bool) {
        if pressed {
            self.pressed |= button.bit();
        } else {
            self.pressed &= !button.bit();
        }
    }

    /// Set the whole button state at once from a raw mask of
    /// [`Button::bit`] values (the frontend ORs gamepads and mouse
    /// presses together; bits above bit 4 are ignored).
    pub fn set_mask(&mut self, mask: u8) {
        self.pressed = mask & 0x1F;
    }

    /// Whether a button is currently held down (for UI feedback).
    pub fn is_pressed(&self, button: Button) -> bool {
        self.pressed & button.bit() != 0
    }

    /// The raw pressed mask (for UI feedback).
    pub fn pressed_mask(&self) -> u8 {
        self.pressed
    }

    /// The external pin levels the 8255 sees on the connector:
    /// pressed buttons pull their line low, idle lines and the
    /// unwired bits 5-7 read high. Only meaningful while the program
    /// has opened the driver's direction transistor (PC4/PC0 — see
    /// [`crate::machine`]).
    pub fn lines(&self) -> u8 {
        !(self.pressed & 0x1F) | 0xE0
    }
}

// The 0xFF & !mask form documents that the upper bits read high;
// clippy sees it as a no-op.
#[cfg(test)]
#[allow(clippy::identity_op)]
mod tests {
    use super::*;

    #[test]
    fn idle_reads_all_lines_high() {
        let stick = Joystick::new();
        assert_eq!(stick.lines(), 0xFF);
    }

    #[test]
    fn pressed_buttons_pull_lines_low() {
        let mut stick = Joystick::new();
        stick.set_button(Button::Up, true);
        stick.set_button(Button::Fire, true);
        // Up = bit 1, fire = bit 4; bits 5-7 stay high.
        assert_eq!(stick.lines(), 0xFF & !0x12);
    }

    #[test]
    fn all_buttons_pressed() {
        let mut stick = Joystick::new();
        for &b in &Button::ALL {
            stick.set_button(b, true);
        }
        assert_eq!(stick.lines(), 0xE0);
    }

    #[test]
    fn set_mask_and_is_pressed_roundtrip() {
        let mut stick = Joystick::new();
        stick.set_mask(Button::Down.bit() | Button::Left.bit() | 0xE0);
        assert!(stick.is_pressed(Button::Down));
        assert!(stick.is_pressed(Button::Left));
        assert!(!stick.is_pressed(Button::Up));
        assert!(!stick.is_pressed(Button::Right));
        assert!(!stick.is_pressed(Button::Fire));
        assert_eq!(stick.pressed_mask(), 0x09);
        assert_eq!(stick.lines(), 0xF6);
    }

    #[test]
    fn reset_releases_everything() {
        let mut stick = Joystick::new();
        stick.set_button(Button::Fire, true);
        stick.reset();
        assert_eq!(stick.lines(), 0xFF);
        assert!(!stick.is_pressed(Button::Fire));
    }
}
