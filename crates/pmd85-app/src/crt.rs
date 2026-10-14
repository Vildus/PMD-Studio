//! CRT simulation: CPU post-processing of the decoded screen
//! framebuffer, applied in [`crate::app::App::refresh_screen`]
//! before the texture upload.
//!
//! Three effects, all plain pixel math (deterministic, unit-tested):
//!
//! - **Scanlines** — each emulated line becomes two texels, the
//!   lower one darkened, so at the usual integer display scales
//!   (2×, 3×, …) the dark line stays a single device pixel tall,
//!   like the gap between scanlines on a real monitor. Below 2× the
//!   strength fades out (a half-pixel line would alias into an ugly
//!   stripe), so the plain 1× image stays untouched.
//! - **Glow** — a separable gaussian blur of the framebuffer is
//!   blended back over it, brightening lit pixels and bleeding
//!   them into the dark surroundings (phosphor bloom). The blend
//!   adds proportionally less the brighter the base already is, so
//!   nothing ever clips.
//! - **Vignette** — a soft radial darkening towards the corners.
//!
//! The output doubles the height when the effect is on (two texels
//! per emulated line); the screen panel stretches the texture back
//! to the emulated aspect, so the image itself is unchanged.

use crate::config::CrtSettings;
use pmd85_core::vram::{HEIGHT, WIDTH};

/// Render the CRT effect over the decoded framebuffer `src`
/// (WIDTH*HEIGHT*4 bytes) into the reusable `dst`, returning it:
/// WIDTH by 2*HEIGHT texels. `scale` is the integer scale the screen
/// panel drew at (last frame); it only gates the scanline strength.
pub fn render<'a>(src: &[u8], dst: &'a mut Vec<u8>, crt: &CrtSettings, scale: f32) -> &'a [u8] {
    debug_assert_eq!(src.len(), WIDTH * HEIGHT * 4);
    dst.clear();
    dst.reserve(WIDTH * HEIGHT * 4 * 2);

    // A dark line below 2x would land between device pixels and
    // alias; fade the scanlines in from 2x up.
    let scanline = crt.scanlines * (scale - 1.0).clamp(0.0, 1.0);

    // ---- scanlines: expand each row into two subrows ----
    for y in 0..HEIGHT {
        let row = &src[y * WIDTH * 4..(y + 1) * WIDTH * 4];
        for factor in [1.0f32, 1.0 - scanline] {
            if factor >= 1.0 {
                // The common un-darkened case: plain copy.
                dst.extend_from_slice(row);
            } else {
                let (pixels, _) = row.as_chunks::<4>();
                for px in pixels {
                    dst.extend_from_slice(&[
                        (px[0] as f32 * factor) as u8,
                        (px[1] as f32 * factor) as u8,
                        (px[2] as f32 * factor) as u8,
                        0xFF,
                    ]);
                }
            }
        }
    }

    let height = HEIGHT * 2;

    // ---- glow: separable [1,4,6,4,1]/16 blur, blended back ----
    if crt.glow > 0.0 {
        // Horizontal then vertical pass (into scratch buffers — the
        // base stays in `dst` for the blend).
        let mut scratch = vec![0u8; dst.len()];
        let mut blur = vec![0u8; dst.len()];
        blur_pass(dst, &mut scratch, WIDTH, height, true);
        blur_pass(&scratch, &mut blur, WIDTH, height, false);
        // Blend: add glow * blur * (1 - base), so black gains the
        // halo, white stays put, and nothing clips.
        for (base, halo) in dst.iter_mut().zip(blur) {
            let add = crt.glow * halo as f32 * (255.0 - *base as f32) / 255.0;
            *base = (*base as f32 + add).min(255.0) as u8;
        }
    }

    // ---- vignette ----
    if crt.vignette > 0.0 {
        let (cx, cy) = ((WIDTH - 1) as f32 / 2.0, (height - 1) as f32 / 2.0);
        for y in 0..height {
            // Rows are uniform horizontally in the falloff rate, but
            // the horizontal part still varies per pixel; compute
            // the row factor outside the x loop.
            let dy = (y as f32 - cy) / cy;
            for x in 0..WIDTH {
                let dx = (x as f32 - cx) / cx;
                // Quadratic falloff from the center, full strength
                // at the corners (dx² + dy² = 2 there).
                let t = ((dx * dx + dy * dy) / 2.0).min(1.0);
                let factor = 1.0 - crt.vignette * t * t;
                let i = (y * WIDTH + x) * 4;
                for c in 0..3 {
                    dst[i + c] = (dst[i + c] as f32 * factor) as u8;
                }
            }
        }
    }

    dst
}

/// One separable blur pass (gaussian [1,4,6,4,1]/16, edges
/// clamped) of an RGBA buffer from `src` into `dst`.
fn blur_pass(src: &[u8], dst: &mut [u8], width: usize, height: usize, horizontal: bool) {
    // Kernel taps relative to the current pixel, in elements of
    // `stride` (one RGBA pixel horizontally, one row vertically).
    const TAPS: [(isize, f32); 5] = [(-2, 1.0), (-1, 4.0), (0, 6.0), (1, 4.0), (2, 1.0)];
    let stride = if horizontal { 4 } else { width * 4 };
    let count = if horizontal { width } else { height };
    for y in 0..height {
        for x in 0..width {
            let base = (y * width + x) * 4;
            let mut sum = [0.0f32; 3];
            for (offset, weight) in TAPS {
                // Clamp the tap to the line's bounds and index
                // relative to the current pixel (the tap is `offset`
                // steps of `stride` away).
                let along = if horizontal { x as isize } else { y as isize };
                let tap = (along + offset).clamp(0, count as isize - 1);
                let i = (base as isize + (tap - along) * stride as isize) as usize;
                for c in 0..3 {
                    sum[c] += src[i + c] as f32 * weight;
                }
            }
            for c in 0..3 {
                dst[base + c] = (sum[c] / 16.0) as u8;
            }
            dst[base + 3] = 0xFF;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crt(enabled: bool, scanlines: f32, glow: f32, vignette: f32) -> CrtSettings {
        CrtSettings {
            enabled,
            scanlines,
            glow,
            vignette,
        }
    }

    /// A deterministic framebuffer: every pixel's RGB from its
    /// coordinates, full alpha.
    fn synthetic() -> Vec<u8> {
        (0..WIDTH * HEIGHT)
            .flat_map(|i| {
                let (x, y) = (i % WIDTH, i / WIDTH);
                [x as u8, y as u8, (x ^ y) as u8, 0xFF]
            })
            .collect()
    }

    #[test]
    fn disabled_effect_duplicates_rows() {
        // Zero strengths: the output is just each row twice.
        let src = synthetic();
        let mut dst = Vec::new();
        let out = render(&src, &mut dst, &crt(true, 0.0, 0.0, 0.0), 3.0);
        assert_eq!(out.len(), WIDTH * HEIGHT * 4 * 2);
        for y in 0..HEIGHT {
            let a = &out[y * WIDTH * 8..(y * 2 + 1) * WIDTH * 4];
            let b = &out[(y * 2 + 1) * WIDTH * 4..(y + 1) * WIDTH * 8];
            assert!(a == b, "subrows differ at y={y}");
            assert_eq!(&src[y * WIDTH * 4..(y + 1) * WIDTH * 4], a);
        }
    }

    #[test]
    fn scanlines_darken_the_lower_subrow() {
        let src = vec![0xFF; WIDTH * HEIGHT * 4];
        let mut dst = Vec::new();
        let out = render(&src, &mut dst, &crt(true, 0.5, 0.0, 0.0), 2.0);
        // Upper subrow: full white; lower: 50%.
        let top = &out[0..4];
        let bottom = &out[WIDTH * 4..WIDTH * 4 + 4];
        assert_eq!(top, &[0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(bottom, &[0x7F, 0x7F, 0x7F, 0xFF]);
    }

    #[test]
    fn scanlines_fade_below_2x_scale() {
        let src = vec![0xFF; WIDTH * HEIGHT * 4];
        let mut dst = Vec::new();
        // At 1x the dark line would alias: no darkening.
        let out = render(&src, &mut dst, &crt(true, 1.0, 0.0, 0.0), 1.0);
        assert_eq!(&out[WIDTH * 4..WIDTH * 4 + 4], &[0xFF; 4]);
    }

    #[test]
    fn glow_bleeds_bright_pixels_into_dark_neighbors() {
        // One bright pixel in a black field.
        let mut src = vec![0u8; WIDTH * HEIGHT * 4];
        for c in 0..3 {
            src[((HEIGHT / 2) * WIDTH + WIDTH / 2) * 4 + c] = 0xFF;
        }
        let mut dst = Vec::new();
        let out = render(&src, &mut dst, &crt(true, 0.0, 1.0, 0.0), 2.0);
        // The dark neighbor right of the bright pixel picked up a
        // halo; the far corner stayed black.
        let px = |x: usize, y: usize| {
            let i = (y * WIDTH + x) * 4;
            [out[i], out[i + 1], out[i + 2]]
        };
        let halo = px(WIDTH / 2 + 1, HEIGHT);
        assert!(
            halo.iter().any(|&c| c > 0),
            "the neighbor of a bright pixel glows: {halo:?}"
        );
        let corner = px(0, 0);
        assert_eq!(corner, [0, 0, 0], "the far corner stays black");
        // A lit pixel itself is not dimmed (the blend only adds).
        let center = px(WIDTH / 2, HEIGHT);
        assert!(center.iter().all(|&c| c == 0xFF), "{center:?}");
    }

    #[test]
    fn vignette_darkens_corners_not_the_center() {
        // A uniform mid-gray screen with a proper opaque alpha.
        let mut src = vec![0x80u8; WIDTH * HEIGHT * 4];
        let (pixels, _) = src.as_chunks_mut::<4>();
        for px in pixels {
            px[3] = 0xFF;
        }
        let mut dst = Vec::new();
        let out = render(&src, &mut dst, &crt(true, 0.0, 0.0, 1.0), 2.0);
        let px = |x: usize, y: usize| {
            let i = (y * WIDTH + x) * 4;
            [out[i], out[i + 1], out[i + 2]]
        };
        let center = px(WIDTH / 2, HEIGHT);
        let corner = px(0, 0);
        assert_eq!(center, [0x80, 0x80, 0x80], "the center is untouched");
        assert!(
            corner[0] < center[0],
            "the corner is darker: {corner:?} vs {center:?}"
        );
        // Alpha survives everything.
        assert_eq!(out[3], 0xFF);
    }
}
