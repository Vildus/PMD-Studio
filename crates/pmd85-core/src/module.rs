//! ROM-module (`.rmm`) images: the 12-byte header the monitor BIOS
//! executes plus the payload bytes behind it.
//!
//! The module connector exposes an 8255 (ports 0xF8-0xFB) through
//! which the monitor can read the module EPROM at any 16-bit
//! address. At boot the monitor copies the module's first 14 bytes
//! into its variable area at 0xC1B2, checks the first byte (the
//! opcode of a CALL) and jumps there — the copied header then calls
//! the monitor's own copy routine (`8C00` on the 85-1/2/2A, `EC00`
//! on the 85-3) with an inline parameter block, which transfers the
//! payload to RAM and executes the trailing JMP into the program.
//! The whole dance is the monitor's code; the emulator only presents
//! module bytes on port reads (see `machine`'s ROM-module 8255).
//!
//! Header layout (12 bytes):
//!
//! ```text
//! +0  CALL <copy routine>   ; CD 00 8C — or CC 00 EC on the 85-3:
//!                          ; the boot reaches it through JZ, so the
//!                          ; call-if-zero fires (and the byte is the
//!                          ; signature the boot CPI checks for)
//! +3  source (word, LE)    ; module offset to copy from (12: the
//!                          ; payload right behind the header)
//! +5  count (word, LE)     ; bytes to copy, minus the routine's bias
//! +7  target (word, LE)    ; RAM address to copy to
//! +9  JMP entry           ; runs after the copy
//! ```
//!
//! The copy routines differ subtly in how the count register is
//! biased: Monitor 1's loop exits without one, copying `count - 0xFF`
//! bytes, while Monitors 2/2A/3 increment the high byte first and
//! copy `count + 1`. The count is encoded accordingly per model.
//!
//! Monitor 1 has no boot-time detection: its `BASIC G` command does
//! a fixed first-stage transfer (module offset 0 to RAM 0) followed
//! by `RST 0`, which executes the copied header — so the same image
//! format works, provided the payload stays clear of the header's
//! twelve bytes at the bottom of RAM (offset 0x10 up).

use crate::model::Model;

/// The module header size in bytes.
pub const HEADER_LEN: usize = 12;

/// The monitor's copy routine the header calls, per model.
fn copy_routine(model: Model) -> (u16, u8) {
    match model {
        // CALL 8C00 with the plain opcode; the 85-3's boot dispatch
        // arrives through JZ, so its header needs CALL-if-zero and
        // the byte doubles as the boot's module signature.
        Model::Pmd851 | Model::Pmd852 | Model::Pmd852a => (0x8C00, 0xCD),
        Model::Pmd853 => (0xEC00, 0xCC),
    }
}

/// Build a ROM-module image: the 12-byte header followed by
/// `payload`. `load_addr` is where the payload lands in RAM (the
/// program's ORG), `entry` where the CPU starts once the transfer
/// finishes (the build's `END` entry point, or the load address).
///
/// The payload must fit a 32 KB module (the connector's address
/// latch is gated at 0x8000) and must live in the RAM below the
/// monitors' boot-time stack use — the caller is expected to check
/// both and tell the user.
pub fn make_rom_module(model: Model, payload: &[u8], load_addr: u16, entry: u16) -> Vec<u8> {
    let len = payload.len() as u16;
    // The count the copy routine needs to transfer exactly `len`
    // bytes: Monitor 1 copies count-0xFF bytes, the others count+1.
    let count = match model {
        Model::Pmd851 => len + 0xFF,
        Model::Pmd852 | Model::Pmd852a | Model::Pmd853 => len - 1,
    };
    let (routine, opcode) = copy_routine(model);
    let mut image = Vec::with_capacity(HEADER_LEN + payload.len());
    image.push(opcode);
    image.extend_from_slice(&routine.to_le_bytes()); // CALL <routine>
    image.extend_from_slice(&(HEADER_LEN as u16).to_le_bytes()); // source
    image.extend_from_slice(&count.to_le_bytes()); // count
    image.extend_from_slice(&load_addr.to_le_bytes()); // target
    image.push(0xC3); // JMP entry
    image.extend_from_slice(&entry.to_le_bytes());
    debug_assert_eq!(image.len(), HEADER_LEN);
    image.extend_from_slice(payload);
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout_matches_the_monitors() {
        // 85-1/2/2A: CALL 8C00 (CD 00 8C), 12 bytes of header.
        let m = make_rom_module(Model::Pmd852a, &[0x76], 0x1000, 0x1000);
        assert_eq!(
            &m[..12],
            &[0xCD, 0x00, 0x8C, 0x0C, 0x00, 0x00, 0x00, 0x00, 0x10, 0xC3, 0x00, 0x10]
        );
        assert_eq!(&m[12..], &[0x76]);
        // Monitor 1's count bias: 1 byte needs count 0x100.
        let m = make_rom_module(Model::Pmd851, &[0x76], 0x1000, 0x1000);
        assert_eq!(
            &m[..12],
            &[0xCD, 0x00, 0x8C, 0x0C, 0x00, 0x00, 0x01, 0x00, 0x10, 0xC3, 0x00, 0x10]
        );
        // The 85-3: CALL-if-zero EC00 (the boot's CPI CC signature).
        let m = make_rom_module(Model::Pmd853, &[0x76; 0x100], 0x2000, 0x2000);
        assert_eq!(&m[..3], &[0xCC, 0x00, 0xEC]);
        assert_eq!(&m[3..9], &[0x0C, 0x00, 0xFF, 0x00, 0x00, 0x20]);
        assert_eq!(&m[9..12], &[0xC3, 0x00, 0x20]);
        // A payload longer than 255 crosses the count's high byte.
        let m = make_rom_module(Model::Pmd852, &[0; 0x101], 0, 0);
        assert_eq!(&m[5..7], &[0x00, 0x01]);
    }

    #[test]
    fn entry_can_differ_from_the_load_address() {
        let m = make_rom_module(Model::Pmd852a, &[0; 4], 0x2000, 0x2003);
        assert_eq!(&m[9..12], &[0xC3, 0x03, 0x20]);
        assert_eq!(&m[7..9], &[0x00, 0x20]);
    }
}
