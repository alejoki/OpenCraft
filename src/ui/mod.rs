pub mod hud;
pub mod inventory;
pub mod lifecycle;
pub mod logo;
pub mod menu;
pub mod quad;
pub mod screens;
pub mod text;
pub mod text_input;

pub use inventory::{
    hit_test as hit_test_inventory, item_color, layout as inventory_layout, screen_labels,
    screen_quads, target_rect as inventory_target_rect, InventoryLabel, InventoryLayout,
    InventoryMode, InventoryScreen, InventoryTarget, MAX_LABELS,
};
pub use lifecycle::{
    decide as decide_lifecycle, LifecycleDecision, LifecycleError, LifecycleIntent,
    LifecycleResult, LifecycleScreen, LifecycleState, LoadGameIntent, MainMenuIntent,
    NewGameIntent, PauseIntent, Screen, SettingsCaller, SettingsIntent,
};
pub use screens::{
    delete_prompt_line, format_modified_secs, format_row_line, hovered_load_target,
    hovered_main_menu_row, hovered_new_game_target, hovered_pause_row, load_back_rect,
    load_cancel_rect, load_confirm_rect, load_delete_rect, load_panel_rect, load_play_rect,
    load_row_rect, main_menu_panel_rect, main_menu_row_rect, new_game_panel_rect,
    new_game_row_lines, new_game_row_rect, pause_panel_rect, pause_row_rect, screen_key_action,
    validate_create, FocusField, LoadScreen, LoadTarget, MainMenuRow, NewGameFormError,
    NewGameScreen, NewGameTarget, PauseRow, ScreenKeyAction, LOAD_SCREEN_WIDTH, MAIN_MENU_ROWS,
    NEW_GAME_ROWS, PAUSE_ROWS, SCREEN_WIDTH, SEED_MAX_CHARS,
};
pub use text_input::{TextInput, TextInputAction, TextInputOutcome};

use crate::pack::PackLogo;
use crate::render::context::RenderContext;
use crate::render::pipeline::UiPass;

pub use quad::{Quad, QuadRenderer};
pub use text::{StatsOverlay, FONT_SIZE, LINE_HEIGHT};

/// Owns the UI rendering stack for the color-only pass: filled quads
/// (tint/panels/buttons), the optional pack logo, plus the glyphon text
/// renderer.
///
/// Created once at startup: `FontSystem::new()` scans system fonts, which is
/// too expensive to do per frame.
pub struct UiRenderer {
    pub font_system: glyphon::FontSystem,
    pub swash_cache: glyphon::SwashCache,
    pub atlas: glyphon::TextAtlas,
    pub text_renderer: glyphon::TextRenderer,
    quad_renderer: QuadRenderer,
    logo_renderer: logo::LogoRenderer,
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
            logo_renderer: logo::LogoRenderer::new(ctx),
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

    /// Replace the pack logo texture. Call on pack load/switch only, never
    /// per frame: the upload is the expensive part of the logo pipeline.
    pub fn set_logo(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        logo: Option<&PackLogo>,
    ) {
        self.logo_renderer.set_logo(device, queue, logo);
    }

    /// Rebuild the per-frame logo quad. `None` clears it, which also keeps a
    /// logo from drawing on non-vista screens.
    pub fn set_logo_frame(
        &mut self,
        device: &wgpu::Device,
        rect: Option<(f32, f32, f32, f32)>,
        surface_w: u32,
        surface_h: u32,
    ) {
        self.logo_renderer
            .set_logo_frame(device, rect, surface_w, surface_h);
    }
}

impl UiPass for UiRenderer {
    fn draw_ui<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        // Quads first (panorama tint/panels), then the pack logo, then text
        // on top.
        self.quad_renderer.draw(pass);
        self.logo_renderer.draw(pass);
        let _ = self.text_renderer.render(&self.atlas, pass);
    }
}
