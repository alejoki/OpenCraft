pub mod text;

use crate::render::context::RenderContext;
use crate::render::pipeline::UiPass;

pub use text::StatsOverlay;

/// Owns the glyphon rendering stack for the UI pass.
///
/// Created once at startup: `FontSystem::new()` scans system fonts, which is
/// too expensive to do per frame.
pub struct UiRenderer {
    pub font_system: glyphon::FontSystem,
    pub swash_cache: glyphon::SwashCache,
    pub atlas: glyphon::TextAtlas,
    pub text_renderer: glyphon::TextRenderer,
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
        }
    }
}

impl UiPass for UiRenderer {
    fn draw_ui<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        let _ = self.text_renderer.render(&self.atlas, pass);
    }
}
