//! Pure pixel-space HUD geometry.
//!
//! No GPU or window types appear here: callers feed surface dimensions and
//! append the returned [`Quad`]s to the existing UI quad list.

use crate::ui::quad::Quad;

/// Width of the crosshair mining progress bar in physical pixels.
pub const PROGRESS_BAR_WIDTH: f32 = 120.0;
/// Height of the crosshair mining progress bar in physical pixels.
pub const PROGRESS_BAR_HEIGHT: f32 = 8.0;
/// Vertical offset of the bar's top edge below the surface center.
pub const CROSSHAIR_BAR_OFFSET_Y: f32 = 24.0;

/// Build the two quads that draw the mining progress bar: an always-visible
/// dark background followed by the amber fill scaled to `progress`.
///
/// Returns `[background, fill]` so the fill is drawn on top.
pub fn mining_progress_quads(progress: f32, surface_w: u32, surface_h: u32) -> [Quad; 2] {
    let progress = if progress.is_nan() {
        0.0
    } else {
        progress.clamp(0.0, 1.0)
    };

    // `max(1)` keeps the centering math finite on a zero-sized surface; on
    // narrower-than-bar surfaces the negative x clips symmetrically.
    let x = (surface_w.max(1) as f32 - PROGRESS_BAR_WIDTH) / 2.0;
    let y = surface_h.max(1) as f32 / 2.0 + CROSSHAIR_BAR_OFFSET_Y;

    let background = Quad {
        x,
        y,
        w: PROGRESS_BAR_WIDTH,
        h: PROGRESS_BAR_HEIGHT,
        color: [0.0, 0.0, 0.0, 0.55],
    };
    let fill = Quad {
        x,
        y,
        w: PROGRESS_BAR_WIDTH * progress,
        h: PROGRESS_BAR_HEIGHT,
        color: [0.95, 0.85, 0.25, 0.95],
    };

    [background, fill]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-4,
            "{what}: {actual} != {expected}"
        );
    }

    #[test]
    fn background_is_centered_below_crosshair() {
        let [bg, _] = mining_progress_quads(0.0, 1280, 720);
        assert_close(bg.x, 580.0, "background x");
        assert_close(bg.y, 384.0, "background y");
        assert_close(bg.w, PROGRESS_BAR_WIDTH, "background w");
        assert_close(bg.h, PROGRESS_BAR_HEIGHT, "background h");
    }

    #[test]
    fn fill_width_is_proportional_and_aligned_with_background() {
        for (progress, expected_w) in [(0.5, 60.0), (0.25, 30.0)] {
            let [bg, fill] = mining_progress_quads(progress, 1280, 720);
            assert_close(fill.w, expected_w, "fill w");
            assert_close(fill.x, bg.x, "fill x");
            assert_close(fill.y, bg.y, "fill y");
            assert_close(fill.h, bg.h, "fill h");
        }
    }

    #[test]
    fn progress_is_clamped_to_unit_range() {
        let [_, full] = mining_progress_quads(1.5, 1280, 720);
        assert_close(full.w, PROGRESS_BAR_WIDTH, "clamped high");

        let [_, negative] = mining_progress_quads(-1.0, 1280, 720);
        assert_close(negative.w, 0.0, "clamped low");

        let [_, nan] = mining_progress_quads(f32::NAN, 1280, 720);
        assert_close(nan.w, 0.0, "nan clamped to zero");
    }

    #[test]
    fn degenerate_surfaces_stay_finite() {
        for (w, h) in [(0, 0), (80, 60)] {
            let quads = mining_progress_quads(0.5, w, h);
            for quad in quads {
                assert!(quad.x.is_finite(), "x finite on {w}x{h}");
                assert!(quad.y.is_finite(), "y finite on {w}x{h}");
                assert!(quad.w.is_finite(), "w finite on {w}x{h}");
                assert!(quad.h.is_finite(), "h finite on {w}x{h}");
            }
        }

        // Narrow surface: symmetric clipping rather than a panic.
        let [bg, _] = mining_progress_quads(0.5, 80, 60);
        assert_close(bg.x, -20.0, "narrow background x");
        assert!(bg.x.is_finite());
    }

    #[test]
    fn background_and_fill_colors_differ_with_valid_alpha() {
        let [bg, fill] = mining_progress_quads(0.5, 1280, 720);
        assert_ne!(bg.color, fill.color, "colors differ");
        for (name, color) in [("background", bg.color), ("fill", fill.color)] {
            for channel in color {
                assert!(
                    (0.0..=1.0).contains(&channel),
                    "{name} channel {channel} out of range"
                );
            }
        }
    }
}
