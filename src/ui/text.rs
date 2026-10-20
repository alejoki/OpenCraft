use glyphon::{Attrs, Buffer, Color, Family, Metrics, Shaping, TextArea};

/// Reusable text buffer for the live stats overlay.
///
/// `update` is driven at the stats cadence (0.25 s), not per frame:
/// `set_text` costs roughly 0.1-1.3 ms per call, so text is only re-shaped
/// when the stats string or the buffer size actually changed.
pub struct StatsOverlay {
    buffer: Buffer,
    last_text: String,
    last_width: f32,
    last_height: f32,
}

// Text metrics for a compact one-line stats readout. Public so the frame
// loop can size the backing panel quad from the same metrics.
pub const FONT_SIZE: f32 = 18.0;
pub const LINE_HEIGHT: f32 = 22.0;

impl StatsOverlay {
    pub fn new(font_system: &mut glyphon::FontSystem) -> Self {
        Self::with_metrics(font_system, FONT_SIZE, LINE_HEIGHT)
    }

    /// Builds an overlay with explicit text metrics, so HUD elements with a
    /// different visual weight (e.g. per-slot hotbar counts) reuse the same
    /// change-gated buffer without duplicating its logic.
    pub fn with_metrics(
        font_system: &mut glyphon::FontSystem,
        font_size: f32,
        line_height: f32,
    ) -> Self {
        Self {
            buffer: Buffer::new(font_system, Metrics::new(font_size, line_height)),
            last_text: String::new(),
            last_width: 0.0,
            last_height: 0.0,
        }
    }

    /// Update the overlay text if anything changed. `width`/`height` are the
    /// surface size in physical pixels (same space as the glyphon `Resolution`).
    pub fn update(
        &mut self,
        font_system: &mut glyphon::FontSystem,
        width: f32,
        height: f32,
        text: &str,
    ) {
        let size_changed = self.last_width != width || self.last_height != height;
        let text_changed = self.last_text != text;
        if !size_changed && !text_changed {
            return;
        }

        self.buffer.set_size(font_system, width, height);
        if text_changed {
            self.buffer.set_text(
                font_system,
                text,
                Attrs::new().family(Family::Monospace),
                Shaping::Advanced,
            );
            self.last_text = text.to_string();
        }
        self.last_width = width;
        self.last_height = height;
        self.buffer.shape_until_scroll(font_system);
    }

    /// Build the glyphon text area for this overlay. `left`/`top` are physical
    /// pixels from the surface's top-left corner.
    pub fn text_area(&self, left: f32, top: f32) -> TextArea<'_> {
        TextArea {
            buffer: &self.buffer,
            left,
            top,
            scale: 1.0,
            bounds: glyphon::TextBounds::default(),
            default_color: Color::rgba(255, 255, 255, 255),
        }
    }
}
