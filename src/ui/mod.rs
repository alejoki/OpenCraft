pub mod quad;
pub mod text;

use crate::render::context::RenderContext;
use crate::render::pipeline::UiPass;

pub use quad::{Quad, QuadRenderer};
pub use text::{StatsOverlay, FONT_SIZE, LINE_HEIGHT};

/// Owns the UI rendering stack for the color-only pass: filled quads
/// (panels/buttons) plus the glyphon text renderer.
///
/// Created once at startup: `FontSystem::new()` scans system fonts, which is
/// too expensive to do per frame.
pub struct UiRenderer {
    pub font_system: glyphon::FontSystem,
    pub swash_cache: glyphon::SwashCache,
    pub atlas: glyphon::TextAtlas,
    pub text_renderer: glyphon::TextRenderer,
    quad_renderer: QuadRenderer,
}

impl UiRenderer {
    pub fn new(ctx: &RenderContext) -> Self {
        // ColorMode::Accurate: proper sRGB texture, matching our sRGB surface format.
        // (glyphon 0.5.0 has no `Color` variant; Accurate/Web are the only modes.)
        let mut atlas = glyphon::TextAtlas::with_color_mode(
            &ctx.device,
            &ctx.queue,
            ctx.config.format,
            glyphon::ColorMode::Accurate,
        );
        // depth_stencil is None: the UI pass is color-only (no depth attachment),
        // and a mismatched depth-stencil pipeline would fail render-pass validation.
        let text_renderer = glyphon::TextRenderer::new(
            &mut atlas,
            &ctx.device,
            wgpu::MultisampleState::default(),
            None,
        );
        Self {
            font_system: glyphon::FontSystem::new(),
            swash_cache: glyphon::SwashCache::new(),
            atlas,
            text_renderer,
            quad_renderer: QuadRenderer::new(ctx),
        }
    }

    /// Rebuild the per-frame quad vertex buffer (panels/buttons). Pure CPU;
    /// call once per frame before the render pass opens. An empty list
    /// clears the buffer so nothing is drawn.
    pub fn set_quads(
        &mut self,
        device: &wgpu::Device,
        quads: &[Quad],
        surface_w: u32,
        surface_h: u32,
    ) {
        self.quad_renderer
            .set_quads(device, quads, surface_w, surface_h);
    }
}

impl UiPass for UiRenderer {
    fn draw_ui<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        // Quads first (panels), then text on top.
        self.quad_renderer.draw(pass);
        let _ = self.text_renderer.render(&self.atlas, pass);
    }
}
