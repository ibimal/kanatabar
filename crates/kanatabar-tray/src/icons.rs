//! Menu-bar status-icon glyphs (SPEC §8).
//!
//! The glyphs are designed as monochrome **template** SVGs
//! (`assets/menubar/*.svg` — a rounded "keycap" with a per-state mark),
//! pre-rasterized to 36px PNGs (`assets/menubar/*.png`, 18pt @2x = Apple's
//! menu-bar size) and decoded here to RGBA. macOS template images carry the
//! shape in the **alpha channel only**, so RGB is forced to black; `tray-icon`'s
//! `with_icon_as_template(true)` then tints them white/black to match the menu
//! bar's appearance.
//!
//! Re-rasterize after editing an SVG with a real SVG renderer that supports
//! masks + `currentColor` (macOS `qlmanage` does **not** — it renders these
//! blank). E.g. resvg: substitute `currentColor`→`#000000` then render at 36px
//! to a transparent PNG. (`assets/menubar/running-preset.*` is a spare "active
//! preset" badge variant, not yet wired to a state.)

use crate::model::IconKind;

/// Square edge of the glyph, in pixels — the size the PNGs are authored at.
pub const ICON_SIZE: u32 = 36;

const RUNNING: &[u8] = include_bytes!("../assets/menubar/running.png");
const PAUSED: &[u8] = include_bytes!("../assets/menubar/paused.png");
const IDLE: &[u8] = include_bytes!("../assets/menubar/idle.png");
const DEGRADED: &[u8] = include_bytes!("../assets/menubar/degraded.png");
const DISCONNECTED: &[u8] = include_bytes!("../assets/menubar/disconnected.png");

/// RGBA8 buffer (`ICON_SIZE * ICON_SIZE * 4` bytes) for `kind`, ready for
/// `tray_icon::Icon::from_rgba`.
pub fn rgba_for(kind: IconKind) -> Vec<u8> {
    template_rgba(match kind {
        IconKind::Running => RUNNING,
        IconKind::Paused => PAUSED,
        IconKind::Idle => IDLE,
        IconKind::Degraded => DEGRADED,
        IconKind::Disconnected => DISCONNECTED,
    })
}

/// Decode an embedded template PNG to RGBA and force RGB to black, so only the
/// alpha channel carries the shape (macOS template-image requirement, SPEC §8).
/// The PNGs are build-time assets we control; on the (build-mistake) chance a
/// decode fails, return a blank transparent buffer rather than panic in the UI.
fn template_rgba(png_bytes: &[u8]) -> Vec<u8> {
    let mut rgba = decode_rgba(png_bytes).unwrap_or_else(blank);
    for px in rgba.chunks_exact_mut(4) {
        px[0] = 0;
        px[1] = 0;
        px[2] = 0;
    }
    rgba
}

fn blank() -> Vec<u8> {
    vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize]
}

/// Decode an 8-bit PNG to RGBA. `None` on any decode error or unexpected format.
fn decode_rgba(png_bytes: &[u8]) -> Option<Vec<u8>> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
        .read_info()
        .ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    match info.color_type {
        png::ColorType::Rgba => Some(buf),
        png::ColorType::Rgb => Some(expand(&buf, 3, |p| [p[0], p[1], p[2], 255])),
        png::ColorType::GrayscaleAlpha => Some(expand(&buf, 2, |p| [p[0], p[0], p[0], p[1]])),
        png::ColorType::Grayscale => Some(expand(&buf, 1, |p| [p[0], p[0], p[0], 255])),
        png::ColorType::Indexed => None,
    }
}

/// Map each `stride`-byte source pixel to an RGBA quad.
fn expand(buf: &[u8], stride: usize, f: impl Fn(&[u8]) -> [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(buf.len() / stride * 4);
    for p in buf.chunks_exact(stride) {
        out.extend_from_slice(&f(p));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KINDS: [IconKind; 5] = [
        IconKind::Running,
        IconKind::Paused,
        IconKind::Degraded,
        IconKind::Idle,
        IconKind::Disconnected,
    ];

    #[test]
    fn every_glyph_decodes_to_the_right_buffer_size() {
        for kind in ALL_KINDS {
            assert_eq!(
                rgba_for(kind).len(),
                (ICON_SIZE * ICON_SIZE * 4) as usize,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn every_glyph_is_a_pure_black_alpha_shape() {
        // macOS template-image requirement (SPEC §8): RGB black, shape in alpha.
        for kind in ALL_KINDS {
            let rgba = rgba_for(kind);
            assert!(
                rgba.chunks_exact(4)
                    .all(|px| px[0] == 0 && px[1] == 0 && px[2] == 0),
                "{kind:?} RGB is not black"
            );
            assert!(
                rgba.chunks_exact(4).any(|px| px[3] > 0),
                "{kind:?} draws nothing"
            );
        }
    }

    #[test]
    fn distinct_kinds_produce_distinct_glyphs() {
        assert_ne!(rgba_for(IconKind::Running), rgba_for(IconKind::Paused));
        assert_ne!(rgba_for(IconKind::Running), rgba_for(IconKind::Degraded));
        assert_ne!(rgba_for(IconKind::Running), rgba_for(IconKind::Idle));
        assert_ne!(rgba_for(IconKind::Idle), rgba_for(IconKind::Disconnected));
        assert_ne!(rgba_for(IconKind::Paused), rgba_for(IconKind::Idle));
    }
}
