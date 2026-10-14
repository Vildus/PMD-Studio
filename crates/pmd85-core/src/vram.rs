//! Video RAM decoding: 288x256 pixels from RAM at 0xC000-0xFFFF.
//!
//! Layout (see https://pmd85.borik.net/wiki/VideoRAM):
//! - 256 microrows, each 64 bytes; bytes 0-47 are visible, 16 are scratch.
//! - per byte: bits 0-5 are six pixels (bit 0 leftmost), bits 6-7 are the
//!   color attribute of that six-pixel group.

use crate::bus::Memory;

/// Screen geometry.
pub const WIDTH: usize = 288;
pub const HEIGHT: usize = 256;
/// VRAM base address.
pub const VRAM_BASE: usize = 0xC000;

/// Color profiles for the attribute bits (PMD 85-3 style).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorProfile {
    /// Four colors as seen on the RGB output of a PMD 85-3:
    /// green, red, blue, violet on black.
    #[default]
    Rgb,
    /// Green monochrome (TV output, normal brightness).
    Mono,
    /// The ColorAce hardware mod (see
    /// https://pmd85.borik.net/wiki/ColorAce): each attribute
    /// colors a pair of stacked microrows (even + odd), and the two
    /// attributes mix additively per color gun, giving 7 colors on
    /// black. Pixel resolution stays full; only color cells are
    /// 6x2.
    ColorAce,
}

impl ColorProfile {
    fn palette(self, phosphor: [u8; 3]) -> [[u8; 4]; 4] {
        match self {
            ColorProfile::Rgb => [
                [0x00, 0xE0, 0x30, 0xFF], // green
                [0xE0, 0x30, 0x20, 0xFF], // red
                [0x30, 0x40, 0xF0, 0xFF], // blue
                [0xD0, 0x60, 0xF0, 0xFF], // violet
            ],
            ColorProfile::Mono => {
                // The phosphor color at full brightness, and a dim
                // variant for the half-brightness attribute values.
                let dim = [
                    (phosphor[0] as u16 * 160 / 256) as u8,
                    (phosphor[1] as u16 * 160 / 256) as u8,
                    (phosphor[2] as u16 * 160 / 256) as u8,
                    0xFF,
                ];
                let bright = [phosphor[0], phosphor[1], phosphor[2], 0xFF];
                [bright, bright, dim, dim]
            }
            // Not used directly: ColorAce mixing combines these four
            // base colors through [`colorace_mix`] instead.
            ColorProfile::ColorAce => [
                [0x00, 0xE0, 0x30, 0xFF], // green
                [0xE0, 0x30, 0x20, 0xFF], // red
                [0x30, 0x40, 0xF0, 0xFF], // blue
                [0xD0, 0x60, 0xF0, 0xFF], // violet
            ],
        }
    }
}

/// The ColorAce color of a microrow pair: the even row's and the odd
/// row's 2-bit attributes (each green / red / blue / violet, the
/// 85-3 mapping) mixed additively per color gun. Same-mixing for
/// both rows of the pair — the mod colors the whole 6x2 block, and
/// the two attributes are interchangeable (the hardware ORs the
/// gun contributions). 7 distinct colors + green result from the 16
/// combinations.
fn colorace_mix(attr_even: u8, attr_odd: u8) -> [u8; 4] {
    // Each attribute's own gun contribution (the 85-3 mapping: bit 6
    // red, bit 7 blue, no bits = green) is ORed with the other row's.
    let red = attr_even & 1 != 0 || attr_odd & 1 != 0;
    let blue = attr_even & 2 != 0 || attr_odd & 2 != 0;
    let green = attr_even == 0 || attr_odd == 0;
    match (red, green, blue) {
        (true, false, false) => [0xE0, 0x30, 0x20, 0xFF], // red
        (false, false, true) => [0x30, 0x40, 0xF0, 0xFF], // blue
        (true, false, true) => [0xD0, 0x60, 0xF0, 0xFF], // violet
        (false, true, false) => [0x00, 0xE0, 0x30, 0xFF], // green
        (true, true, false) => [0xE0, 0xE0, 0x30, 0xFF], // yellow
        (false, true, true) => [0x30, 0xE0, 0xF0, 0xFF], // cyan
        (true, true, true) => [0xE0, 0xE0, 0xF0, 0xFF], // white
        (false, false, false) => [0x00, 0x00, 0x00, 0xFF], // black
    }
}

/// Decode the Video RAM into an RGBA framebuffer (WIDTH*HEIGHT*4 bytes).
pub fn decode(mem: &Memory, profile: ColorProfile, phosphor: [u8; 3]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(WIDTH * HEIGHT * 4);
    decode_into(mem, profile, phosphor, &mut buf);
    buf
}

/// Decode into a reusable buffer; returns the buffer for convenience.
/// `phosphor` is the monochrome color of the [`ColorProfile::Mono`]
/// profile (bright; the dim variant is derived) and ignored by the
/// others.
pub fn decode_into<'a>(
    mem: &Memory,
    profile: ColorProfile,
    phosphor: [u8; 3],
    buf: &'a mut Vec<u8>,
) -> &'a mut Vec<u8> {
    buf.clear();
    buf.reserve(WIDTH * HEIGHT * 4);
    let black = [0x00, 0x00, 0x00, 0xFF];
    if profile == ColorProfile::ColorAce {
        // Microrow pairs: the mixed color of the 6x2 block, the
        // pixel bits of both rows. Row order is preserved (the whole
        // even microrow, then the odd one below it).
        for y in (0..HEIGHT).step_by(2) {
            let row = VRAM_BASE + y * 0x40;
            for dy in 0..2 {
                for byte in 0..48 {
                    let v = mem.ram[row + dy * 0x40 + byte];
                    let color = colorace_mix(
                        (mem.ram[row + byte] >> 6) & 3,
                        (mem.ram[row + 0x40 + byte] >> 6) & 3,
                    );
                    for bit in 0..6 {
                        if v & (1 << bit) != 0 {
                            buf.extend_from_slice(&color);
                        } else {
                            buf.extend_from_slice(&black);
                        }
                    }
                }
            }
        }
        return buf;
    }
    let palette = profile.palette(phosphor);
    for y in 0..HEIGHT {
        let row = VRAM_BASE + y * 0x40;
        for byte in 0..48 {
            let v = mem.ram[row + byte];
            let color = palette[(v >> 6) as usize & 3];
            // bit 0 is the leftmost of the six pixels (MAME screen_update)
            for bit in 0..6 {
                if v & (1 << bit) != 0 {
                    buf.extend_from_slice(&color);
                } else {
                    buf.extend_from_slice(&black);
                }
            }
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Model;

    fn mem_with_vram() -> Memory {
        let mut m = Memory::new(Model::Pmd853, &vec![0; 0x2000]);
        // row 0, byte 0: single leftmost pixel, green (attr 00)
        m.ram[VRAM_BASE] = 0b0000_0001;
        // row 0, byte 1: all six pixels, red (attr 01)
        m.ram[VRAM_BASE + 1] = 0b0100_0000 | 0b0011_1111;
        // row 255, byte 47: rightmost pixel, violet (attr 11)
        m.ram[VRAM_BASE + 255 * 0x40 + 47] = 0b1100_0000 | 0b0010_0000;
        // scratch byte: must not be displayed
        m.ram[VRAM_BASE + 48] = 0xFF;
        m
    }

    fn px(buf: &[u8], x: usize, y: usize) -> [u8; 4] {
        let i = (y * WIDTH + x) * 4;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    #[test]
    fn framebuffer_size() {
        let m = mem_with_vram();
        let buf = decode(&m, ColorProfile::Rgb, GREEN);
        assert_eq!(buf.len(), WIDTH * HEIGHT * 4);
    }

    #[test]
    fn pixel_order_and_colors() {
        let m = mem_with_vram();
        let buf = decode(&m, ColorProfile::Rgb, GREEN);
        // bit 0 = leftmost pixel of the byte
        assert_eq!(px(&buf, 0, 0), [0x00, 0xE0, 0x30, 0xFF]); // green on
        assert_eq!(px(&buf, 1, 0), [0, 0, 0, 0xFF]); // off
                                                     // byte 1: full red group
        for x in 6..12 {
            assert_eq!(px(&buf, x, 0), [0xE0, 0x30, 0x20, 0xFF], "x={x}");
        }
        // bit 5 = rightmost pixel
        assert_eq!(px(&buf, 6 * 47 + 5, 255), [0xD0, 0x60, 0xF0, 0xFF]);
        assert_eq!(px(&buf, 6 * 47 + 4, 255), [0, 0, 0, 0xFF]);
        // scratch bytes never displayed: row 0 stops at x=287
        assert_eq!(px(&buf, 287, 0), [0, 0, 0, 0xFF]);
    }

    #[test]
    fn microrow_addressing() {
        // 0xC030 is a scratch address; row 1 starts at 0xC040
        let mut m = mem_with_vram();
        m.ram[VRAM_BASE + 0x40] = 0b1100_0000 | 0b0000_0001; // attr 11, pixel 0
        let buf = decode(&m, ColorProfile::Rgb, GREEN);
        assert_eq!(px(&buf, 0, 1), [0xD0, 0x60, 0xF0, 0xFF]);
    }

    /// The default phosphor used by the tests: today's green.
    const GREEN: [u8; 3] = [0x00, 0xD0, 0x00];

    #[test]
    fn mono_profile_uses_the_phosphor() {
        let mut m = mem_with_vram();
        // attr 01/10 pick the bright/dim variants.
        m.ram[VRAM_BASE + 1] = 0b0100_0000 | 0b0011_1111; // bright
        m.ram[VRAM_BASE + 2] = 0b1000_0000 | 0b0011_1111; // dim
        let buf = decode(&m, ColorProfile::Mono, [0xE0, 0xA0, 0x20]);
        assert_eq!(px(&buf, 6, 0), [0xE0, 0xA0, 0x20, 0xFF]);
        let dim = px(&buf, 12, 0);
        assert_eq!(dim, [0x8C, 0x64, 0x14, 0xFF]);
        assert_eq!(dim[0], (0xE0u16 * 160 / 256) as u8);
    }

    #[test]
    fn colorace_mixes_guns_per_microrow_pair() {
        // Yellow = red in the even microrow + green in the odd one
        // (attrs 01 / 00), per the reference ColorAce picture
        // editor's palette.
        assert_eq!(colorace_mix(0b01, 0b00), [0xE0, 0xE0, 0x30, 0xFF]);
        // Cyan and white mix the same way.
        assert_eq!(colorace_mix(0b10, 0b00), [0x30, 0xE0, 0xF0, 0xFF]);
        assert_eq!(colorace_mix(0b11, 0b00), [0xE0, 0xE0, 0xF0, 0xFF]);
        // The plain four colors come through unmixed.
        assert_eq!(colorace_mix(0b00, 0b00), [0x00, 0xE0, 0x30, 0xFF]);
        assert_eq!(colorace_mix(0b01, 0b01), [0xE0, 0x30, 0x20, 0xFF]);
        assert_eq!(colorace_mix(0b10, 0b10), [0x30, 0x40, 0xF0, 0xFF]);
        assert_eq!(colorace_mix(0b11, 0b11), [0xD0, 0x60, 0xF0, 0xFF]);
        // The two attributes are interchangeable.
        assert_eq!(colorace_mix(0b00, 0b01), colorace_mix(0b01, 0b00));
        assert_eq!(colorace_mix(0b00, 0b11), colorace_mix(0b11, 0b00));
        // ...and there are exactly 7 distinct colors over the 16
        // attribute pairs.
        let distinct: std::collections::HashSet<[u8; 4]> =
            (0..4).flat_map(|a| (0..4).map(move |b| colorace_mix(a, b))).collect();
        assert_eq!(distinct.len(), 7);
    }

    #[test]
    fn colorace_colors_the_pair_keeps_pixels_per_row() {
        let mut m = mem_with_vram();
        // Even microrow 0, byte 0: attr 01, pixel 0 on.
        // Odd microrow 1, byte 0: attr 00, pixel 1 on, pixel 0 off.
        m.ram[VRAM_BASE] = 0b0100_0000 | 0b0000_0001;
        m.ram[VRAM_BASE + 0x40] = 0b0000_0010;
        let buf = decode(&m, ColorProfile::ColorAce, GREEN);
        // Both rows show the mixed yellow; pixel bits stay per-row.
        assert_eq!(px(&buf, 0, 0), [0xE0, 0xE0, 0x30, 0xFF]);
        assert_eq!(px(&buf, 0, 1), [0x00, 0x00, 0x00, 0xFF]);
        assert_eq!(px(&buf, 1, 1), [0xE0, 0xE0, 0x30, 0xFF]);
        // A row that is not part of this pair is unaffected: row 2's
        // byte 0 (attr 00 green, pixel 0 on) still reads green when
        // its odd partner (row 3) is also 00.
        m.ram[VRAM_BASE + 0x80] = 0b0000_0001;
        let buf = decode(&m, ColorProfile::ColorAce, GREEN);
        assert_eq!(px(&buf, 0, 2), [0x00, 0xE0, 0x30, 0xFF]);
    }
}
