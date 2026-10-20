//! Pure-logic settings menu: row model, pixel-space layout, hit-testing,
//! clamped adjustment helpers, and the rebind-capture decision function.
//!
//! No GPU, no event loop, no clock, no filesystem — every input is an
//! argument and every output is derived state, so the whole state machine
//! is unit-testable on CPU alone (the same de-risking approach S03 used
//! with `src/ui/quad.rs`). `main.rs` (S04-T02/T03) owns the frame loop and
//! calls these functions with `ctx.config.width/height` and `cursor_pos`
//! each frame (immediate mode), so a fullscreen `Resized` reflows the menu
//! automatically. All coordinates are physical pixels, top-left origin,
//! y down — the same space as `Quad`, `ctx.config`, glyphon `TextArea`,
//! and `GameApp.cursor_pos` (P003/MEM012).

use winit::keyboard::KeyCode;

use super::LINE_HEIGHT;
use crate::settings::keybind::key_to_string;
use crate::settings::{
    Keybinds, SettingsWarning, FOV_MAX, FOV_MIN, RENDER_DISTANCE_MAX, RENDER_DISTANCE_MIN,
    SENSITIVITY_MAX, SENSITIVITY_MIN,
};

/// Fixed panel width in px. Wide enough for "Mouse Sensitivity: 0.0022"
/// style lines at `FONT_SIZE` 18.
pub const MENU_WIDTH: f32 = 360.0;
/// Panel top edge in px. Leaves room for the stats overlay's top lines.
pub const MENU_TOP: f32 = 60.0;
/// Inner padding between the panel edge and the first/last row, in px.
pub const MENU_PAD: f32 = 8.0;
/// One step of the sensitivity adjustment, within
/// [`SENSITIVITY_MIN`, `SENSITIVITY_MAX`].
pub const SENSITIVITY_STEP: f32 = 0.0005;
/// One step of the FOV adjustment in degrees, within
/// [`FOV_MIN`, `FOV_MAX`].
pub const FOV_STEP: f32 = 5.0;
/// One step of the render-distance adjustment in chunks, within
/// [`RENDER_DISTANCE_MIN`, `RENDER_DISTANCE_MAX`]. Matches the old
/// bracket-key step so the setting preserves the previous feel.
pub const RENDER_DISTANCE_STEP: i32 = 2;
/// Default warning-message lifetime: the caller decrements once per frame,
/// so at 60 fps this is ~10 seconds. The module never owns a clock.
pub const MESSAGE_TICKS: u32 = 600;

/// Rect in physical px: `(x, y, w, h)`, top-left origin, y down.
pub type Rect = (f32, f32, f32, f32);

/// One rebindable gameplay action. `name()` returns the exact action
/// strings used by `Settings::validate_keybinds` and the `[keybinds]` TOML
/// table, keeping the menu's messages aligned with load-time conflict data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindAction {
    Forward,
    Backward,
    Left,
    Right,
    JumpOrUp,
    CrouchOrDown,
    Sprint,
}

impl BindAction {
    /// All 7 actions in `Keybinds` declaration order.
    pub const ALL: [BindAction; 7] = [
        BindAction::Forward,
        BindAction::Backward,
        BindAction::Left,
        BindAction::Right,
        BindAction::JumpOrUp,
        BindAction::CrouchOrDown,
        BindAction::Sprint,
    ];

    /// Stable action name, identical to the strings used by
    /// `Settings::validate_keybinds` and the settings file format.
    pub const fn name(self) -> &'static str {
        match self {
            BindAction::Forward => "forward",
            BindAction::Backward => "backward",
            BindAction::Left => "left",
            BindAction::Right => "right",
            BindAction::JumpOrUp => "jump_or_up",
            BindAction::CrouchOrDown => "crouch_or_down",
            BindAction::Sprint => "sprint",
        }
    }

    /// The key currently bound to this action.
    pub fn key_of(self, binds: &Keybinds) -> KeyCode {
        match self {
            BindAction::Forward => binds.forward,
            BindAction::Backward => binds.backward,
            BindAction::Left => binds.left,
            BindAction::Right => binds.right,
            BindAction::JumpOrUp => binds.jump_or_up,
            BindAction::CrouchOrDown => binds.crouch_or_down,
            BindAction::Sprint => binds.sprint,
        }
    }

    /// Rebind this action to `code`. Pure setter on the passed-in
    /// `Keybinds`; persistence is the caller's job.
    pub fn set_key(self, binds: &mut Keybinds, code: KeyCode) {
        match self {
            BindAction::Forward => binds.forward = code,
            BindAction::Backward => binds.backward = code,
            BindAction::Left => binds.left = code,
            BindAction::Right => binds.right = code,
            BindAction::JumpOrUp => binds.jump_or_up = code,
            BindAction::CrouchOrDown => binds.crouch_or_down = code,
            BindAction::Sprint => binds.sprint = code,
        }
    }
}

/// One selectable menu line. The full list is 7 keybind rows plus
/// sensitivity, FOV, render distance, fullscreen, vsync, and a close row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuRow {
    Bind(BindAction),
    MouseSensitivity,
    Fov,
    RenderDistance,
    Fullscreen,
    Vsync,
    Close,
}

/// All rows top-to-bottom, in menu order.
pub const ROWS: [MenuRow; 13] = [
    MenuRow::Bind(BindAction::Forward),
    MenuRow::Bind(BindAction::Backward),
    MenuRow::Bind(BindAction::Left),
    MenuRow::Bind(BindAction::Right),
    MenuRow::Bind(BindAction::JumpOrUp),
    MenuRow::Bind(BindAction::CrouchOrDown),
    MenuRow::Bind(BindAction::Sprint),
    MenuRow::MouseSensitivity,
    MenuRow::Fov,
    MenuRow::RenderDistance,
    MenuRow::Fullscreen,
    MenuRow::Vsync,
    MenuRow::Close,
];

/// Total number of menu rows.
pub const ROW_COUNT: usize = ROWS.len();

impl MenuRow {
    /// Human-facing row label (without the current value; the caller
    /// appends the value when composing the display line).
    pub const fn label(self) -> &'static str {
        match self {
            MenuRow::Bind(BindAction::Forward) => "Move Forward",
            MenuRow::Bind(BindAction::Backward) => "Move Backward",
            MenuRow::Bind(BindAction::Left) => "Move Left",
            MenuRow::Bind(BindAction::Right) => "Move Right",
            MenuRow::Bind(BindAction::JumpOrUp) => "Jump / Up",
            MenuRow::Bind(BindAction::CrouchOrDown) => "Crouch / Down",
            MenuRow::Bind(BindAction::Sprint) => "Sprint",
            MenuRow::MouseSensitivity => "Mouse Sensitivity",
            MenuRow::Fov => "Field of View",
            MenuRow::RenderDistance => "Render Distance",
            MenuRow::Fullscreen => "Fullscreen",
            MenuRow::Vsync => "VSync",
            MenuRow::Close => "Close Menu",
        }
    }
}

/// The row at `index` (0-based, top-to-bottom), or `None` past the end.
pub fn row(index: usize) -> Option<MenuRow> {
    ROWS.get(index).copied()
}

/// Which-key-capture state machine. `Closed` draws nothing and takes no
/// input; `Open` shows the rows; `AwaitingKey` is capture mode for one
/// action — the next key press is evaluated as that action's new binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuState {
    Closed,
    Open,
    AwaitingKey { action: BindAction },
}

/// A transient in-menu message (duplicate-key warning, unsupported-key
/// warning, ...). Expiry is a tick counter the caller decrements once per
/// frame via [`SettingsMenu::tick`]; the module owns no clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuMessage {
    pub text: String,
    pub ticks_left: u32,
}

/// The whole menu's mutable state. Pure data plus transition methods —
/// no rendering, no events, no clock. The caller drives it from winit
/// events and reads geometry back per frame.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsMenu {
    pub state: MenuState,
    pub message: Option<MenuMessage>,
}

impl Default for SettingsMenu {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsMenu {
    pub fn new() -> Self {
        Self {
            state: MenuState::Closed,
            message: None,
        }
    }

    /// Whether the menu occupies the screen and consumes input
    /// (both `Open` and `AwaitingKey` are "open" from the player's view).
    pub fn is_open(&self) -> bool {
        !matches!(self.state, MenuState::Closed)
    }

    /// The action currently awaiting a key press, if any.
    pub fn awaiting_action(&self) -> Option<BindAction> {
        match self.state {
            MenuState::AwaitingKey { action } => Some(action),
            _ => None,
        }
    }

    /// Escape while closed is a no-op; otherwise the menu opens. The
    /// caller releases the cursor grab and resets `InputState` when this
    /// returns `true`.
    pub fn open(&mut self) -> bool {
        if self.is_open() {
            false
        } else {
            self.state = MenuState::Open;
            true
        }
    }

    /// Close from any state, discarding any pending capture.
    pub fn close(&mut self) {
        self.state = MenuState::Closed;
    }

    /// Enter capture mode for `action` (only sensible while `Open`;
    /// switching directly between two captures is also allowed).
    pub fn begin_rebind(&mut self, action: BindAction) {
        self.state = MenuState::AwaitingKey { action };
    }

    /// Cancel capture (e.g. Escape while `AwaitingKey`): back to `Open`.
    /// Returns `true` if a capture was actually cancelled.
    pub fn cancel_capture(&mut self) -> bool {
        match self.state {
            MenuState::AwaitingKey { .. } => {
                self.state = MenuState::Open;
                true
            }
            _ => false,
        }
    }

    /// Leave capture mode after an accepted rebind: back to `Open`.
    /// Returns `true` if a capture was active. The caller applies the new
    /// key to `Keybinds` and saves *before* calling this.
    pub fn finish_capture(&mut self) -> bool {
        self.cancel_capture()
    }

    /// Show `text` as a transient message for [`MESSAGE_TICKS`] ticks
    /// (caller decrements once per frame via `tick`). Replaces any
    /// previous message.
    pub fn set_message(&mut self, text: impl Into<String>) {
        self.set_message_for_ticks(text, MESSAGE_TICKS);
    }

    /// Show `text` for an explicit tick count (tests use small counts).
    pub fn set_message_for_ticks(&mut self, text: impl Into<String>, ticks: u32) {
        self.message = Some(MenuMessage {
            text: text.into(),
            ticks_left: ticks,
        });
    }

    /// Clear the message immediately.
    pub fn clear_message(&mut self) {
        self.message = None;
    }

    /// One frame tick: decrements the message's lifetime and drops it at
    /// zero. Returns whether a message is still showing.
    pub fn tick(&mut self) -> bool {
        match &mut self.message {
            None => false,
            Some(msg) => {
                msg.ticks_left = msg.ticks_left.saturating_sub(1);
                if msg.ticks_left == 0 {
                    self.message = None;
                    false
                } else {
                    true
                }
            }
        }
    }
}

/// Geometry of row `index` in physical px (top-left origin, y down),
/// derived from the surface size so a `Resized` reflows automatically.
/// Rows are horizontally centered and `LINE_HEIGHT` (22.0) tall.
/// Returns `None` for out-of-range indices.
pub fn row_rect(surface_w: u32, surface_h: u32, index: usize) -> Option<Rect> {
    if index >= ROW_COUNT {
        return None;
    }
    let _ = surface_h; // height not needed for a top-anchored panel
    let w = surface_w as f32;
    // Clamp x at 0: on a surface narrower than the menu, keep the panel
    // rooted to the left edge instead of drifting offscreen negatively.
    let x = ((w - MENU_WIDTH) / 2.0).max(0.0);
    let y = MENU_TOP + MENU_PAD + (index as f32) * LINE_HEIGHT;
    Some((x, y, MENU_WIDTH, LINE_HEIGHT))
}

/// The dark panel rect behind all rows (rows plus padding on every side).
pub fn panel_rect(surface_w: u32, _surface_h: u32) -> Rect {
    let w = surface_w as f32;
    let x = ((w - MENU_WIDTH) / 2.0).max(0.0) - MENU_PAD;
    let y = MENU_TOP;
    let width = MENU_WIDTH + 2.0 * MENU_PAD;
    let height = 2.0 * MENU_PAD + ROW_COUNT as f32 * LINE_HEIGHT;
    (x, y, width, height)
}

/// Pure point-in-rect test in physical px. Half-open like the pixel grid:
/// the left/top edges are inside, the right/bottom edges are outside.
pub fn hit_test(cursor: (f32, f32), rect: Rect) -> bool {
    let (cx, cy) = cursor;
    let (x, y, w, h) = rect;
    cx >= x && cx < x + w && cy >= y && cy < y + h
}

/// Which row the cursor is over, if any. Scans top to bottom; disjoint
/// rows mean at most one match.
pub fn hovered_row(cursor: (f32, f32), surface_w: u32, surface_h: u32) -> Option<MenuRow> {
    (0..ROW_COUNT)
        .find(|&index| {
            row_rect(surface_w, surface_h, index).is_some_and(|rect| hit_test(cursor, rect))
        })
        .map(|index| ROWS[index])
}

/// Clamp a sensitivity value to [`SENSITIVITY_MIN`, `SENSITIVITY_MAX`].
/// Clamping happens in f64 (the settings-file precision) and casts to f32,
/// matching the load path's `load_clamped_f32` exactly.
pub fn clamp_sensitivity(value: f32) -> f32 {
    ((value as f64).clamp(SENSITIVITY_MIN, SENSITIVITY_MAX)) as f32
}

/// Clamp a FOV value to [`FOV_MIN`, `FOV_MAX`] (degrees). In-memory
/// `Settings` are unclamped — only load and this helper clamp — so menu
/// adjustments MUST go through here before saving.
pub fn clamp_fov(value: f32) -> f32 {
    ((value as f64).clamp(FOV_MIN, FOV_MAX)) as f32
}

/// Clamp a render distance to [`RENDER_DISTANCE_MIN`,
/// `RENDER_DISTANCE_MAX`] in chunks. Kept in sync with `WorldManager`'s
/// own clamp so the menu can never request an out-of-range world.
pub fn clamp_render_distance(value: i32) -> i32 {
    value.clamp(RENDER_DISTANCE_MIN, RENDER_DISTANCE_MAX)
}

/// One-step adjustment direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdjustDirection {
    Up,
    Down,
}

/// Step the sensitivity up or down by [`SENSITIVITY_STEP`], clamped to the
/// valid range. Stepping past either bound pins at the bound.
pub fn adjust_sensitivity(current: f32, direction: AdjustDirection) -> f32 {
    let delta = match direction {
        AdjustDirection::Up => SENSITIVITY_STEP,
        AdjustDirection::Down => -SENSITIVITY_STEP,
    };
    clamp_sensitivity(current + delta)
}

/// Step the FOV up or down by [`FOV_STEP`] degrees, clamped to the valid
/// range. Stepping past either bound pins at the bound.
pub fn adjust_fov(current: f32, direction: AdjustDirection) -> f32 {
    let delta = match direction {
        AdjustDirection::Up => FOV_STEP,
        AdjustDirection::Down => -FOV_STEP,
    };
    clamp_fov(current + delta)
}

/// Step the render distance up or down by [`RENDER_DISTANCE_STEP`] chunks,
/// clamped to the valid range. Stepping past either bound pins at the bound.
pub fn adjust_render_distance(current: i32, direction: AdjustDirection) -> i32 {
    let delta = match direction {
        AdjustDirection::Up => RENDER_DISTANCE_STEP,
        AdjustDirection::Down => -RENDER_DISTANCE_STEP,
    };
    clamp_render_distance(current + delta)
}

/// Verdict on a key pressed while capturing a rebind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RebindDecision {
    /// The key is supported and free: rebind `action` to it, then save.
    Accept(KeyCode),
    /// The key has no `key_to_string` mapping. `Settings::save` PANICS on
    /// unmapped keys, so this rejection is mandatory, not cosmetic.
    RejectUnsupported { key: KeyCode },
    /// Another action already holds this key; rebinding would create a
    /// duplicate. `conflicting_action` matches `KeybindConflict.actions`
    /// naming.
    RejectDuplicate { conflicting_action: BindAction },
}

/// Decide what a key press during capture means for `action`.
///
/// Duplicate semantics match `Settings::validate_keybinds`: a conflict is
/// any *other* action already holding the key (pressing the key already
/// bound to `action` itself is an idempotent accept). The unsupported-key
/// filter runs first: `Settings::save` panics on keys outside
/// `keybind::KEY_TABLE`, so those never reach an accept.
pub fn evaluate_rebind_candidate(
    current: &Keybinds,
    action: BindAction,
    pressed: KeyCode,
) -> RebindDecision {
    if key_to_string(pressed).is_none() {
        return RebindDecision::RejectUnsupported { key: pressed };
    }
    for other in BindAction::ALL {
        if other != action && other.key_of(current) == pressed {
            return RebindDecision::RejectDuplicate {
                conflicting_action: other,
            };
        }
    }
    RebindDecision::Accept(pressed)
}

/// Warning text for a rejected rebind, or `None` for an accept. Keeps the
/// decision data and its display wording in one place.
pub fn rebind_rejection_message(decision: &RebindDecision) -> Option<String> {
    match decision {
        RebindDecision::Accept(_) => None,
        RebindDecision::RejectUnsupported { key } => {
            Some(format!("{key:?} is not a supported key"))
        }
        RebindDecision::RejectDuplicate { conflicting_action } => Some(format!(
            "key already bound to '{}'",
            conflicting_action.name()
        )),
    }
}

/// Format load-time [`SettingsWarning`]s as menu display lines
/// (MEM009: warnings are data — surfaced, not logged; the crate is
/// log-free by convention). One line per warning: "field: reason".
pub fn format_warnings(warnings: &[SettingsWarning]) -> Vec<String> {
    warnings
        .iter()
        .map(|w| format!("{}: {}", w.field, w.reason))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SW: u32 = 1280;
    const SH: u32 = 720;

    // --- layout ---

    #[test]
    fn layout_is_deterministic_for_the_same_surface() {
        for index in 0..ROW_COUNT {
            assert_eq!(
                row_rect(SW, SH, index),
                row_rect(SW, SH, index),
                "row {index} rect must be stable across calls"
            );
        }
    }

    #[test]
    fn rows_are_centered_line_height_tall_and_stacked() {
        let (x0, y0, w0, h0) = row_rect(SW, SH, 0).unwrap();
        let expected_x = (SW as f32 - MENU_WIDTH) / 2.0;
        assert_eq!(x0, expected_x);
        assert_eq!(y0, MENU_TOP + MENU_PAD);
        assert_eq!(w0, MENU_WIDTH);
        assert_eq!(h0, LINE_HEIGHT);

        for index in 1..ROW_COUNT {
            let (x, y, w, h) = row_rect(SW, SH, index).unwrap();
            assert_eq!(x, x0, "all rows share the menu x");
            assert_eq!(w, MENU_WIDTH);
            assert_eq!(h, LINE_HEIGHT);
            assert_eq!(y, y0 + index as f32 * LINE_HEIGHT);
        }
    }

    #[test]
    fn row_rect_reflows_with_surface_width_and_clamps_at_narrow_surfaces() {
        let (x_wide, ..) = row_rect(1920, SH, 3).unwrap();
        let (x_default, ..) = row_rect(SW, SH, 3).unwrap();
        assert!(x_wide > x_default, "wider surface shifts rows right");

        // Surface narrower than the menu: panel rooted at x = 0, not negative.
        let (x_narrow, ..) = row_rect(200, SH, 3).unwrap();
        assert_eq!(x_narrow, 0.0);
    }

    #[test]
    fn row_index_out_of_range_returns_none() {
        assert_eq!(row_rect(SW, SH, ROW_COUNT), None);
        assert_eq!(row_rect(SW, SH, 999), None);
        assert_eq!(row(ROW_COUNT), None);
    }

    #[test]
    fn panel_rect_covers_every_row_plus_padding() {
        let (px, py, pw, ph) = panel_rect(SW, SH);
        for index in 0..ROW_COUNT {
            let (x, y, w, h) = row_rect(SW, SH, index).unwrap();
            assert!(
                px <= x && x + w <= px + pw,
                "row {index} inside panel horizontally"
            );
            assert!(
                py <= y && y + h <= py + ph,
                "row {index} inside panel vertically"
            );
        }
        // padding is symmetric and exactly MENU_PAD on each side
        let (x0, y0, ..) = row_rect(SW, SH, 0).unwrap();
        let (_, _, _, h_last) = row_rect(SW, SH, ROW_COUNT - 1).unwrap();
        assert_eq!(x0 - px, MENU_PAD);
        assert_eq!(y0 - py, MENU_PAD);
        let last_bottom = y0 + (ROW_COUNT - 1) as f32 * LINE_HEIGHT + h_last;
        assert_eq!(py + ph - last_bottom, MENU_PAD);
    }

    // --- hit-test ---

    #[test]
    fn hit_test_inside_is_true_and_outside_is_false() {
        let rect = (100.0, 200.0, 50.0, 22.0);
        assert!(hit_test((120.0, 210.0), rect));
        assert!(hit_test((100.0, 200.0), rect), "top-left corner is inside");
        assert!(!hit_test((99.9, 210.0), rect));
        assert!(!hit_test((120.0, 199.9), rect));
        assert!(!hit_test((300.0, 700.0), rect));
        assert!(!hit_test((-1.0, -1.0), rect));
    }

    #[test]
    fn hit_test_boundaries_are_half_open_like_the_pixel_grid() {
        let rect = (100.0, 200.0, 50.0, 22.0);
        assert!(hit_test((100.0, 200.0), rect), "left edge in");
        assert!(hit_test((100.0, 200.0), rect), "top edge in");
        assert!(!hit_test((150.0, 210.0), rect), "right edge out");
        assert!(!hit_test((120.0, 222.0), rect), "bottom edge out");
    }

    #[test]
    fn hovered_row_finds_the_row_under_the_cursor_and_none_elsewhere() {
        let (x, y0, w, h) = row_rect(SW, SH, 4).unwrap();
        let mid_y = y0 + h / 2.0;
        assert_eq!(hovered_row((x + w / 2.0, mid_y), SW, SH), Some(ROWS[4]));
        assert_eq!(hovered_row((0.0, 0.0), SW, SH), None, "above the panel");
        assert_eq!(
            hovered_row((SW as f32 - 1.0, SH as f32 - 1.0), SW, SH),
            None,
            "bottom-right corner is below the last row"
        );
    }

    // --- clamped adjustments ---

    #[test]
    fn sensitivity_steps_move_by_the_step_and_clamp_both_directions() {
        let base = 0.0022;
        assert_eq!(
            adjust_sensitivity(base, AdjustDirection::Up),
            base + SENSITIVITY_STEP
        );
        assert_eq!(
            adjust_sensitivity(base, AdjustDirection::Down),
            base - SENSITIVITY_STEP
        );

        // near the top: 0.0099 + 0.0005 would be 0.0104 -> clamps to MAX
        assert_eq!(
            adjust_sensitivity(0.0099, AdjustDirection::Up),
            SENSITIVITY_MAX as f32
        );
        // at the top: stays pinned
        assert_eq!(
            adjust_sensitivity(SENSITIVITY_MAX as f32, AdjustDirection::Up),
            SENSITIVITY_MAX as f32
        );
        // near the bottom: 0.0002 - 0.0005 would be negative -> clamps to MIN
        assert_eq!(
            adjust_sensitivity(0.0002, AdjustDirection::Down),
            SENSITIVITY_MIN as f32
        );
        assert_eq!(
            adjust_sensitivity(SENSITIVITY_MIN as f32, AdjustDirection::Down),
            SENSITIVITY_MIN as f32
        );
    }

    #[test]
    fn fov_steps_move_by_five_degrees_and_clamp_both_directions() {
        assert_eq!(adjust_fov(75.0, AdjustDirection::Up), 80.0);
        assert_eq!(adjust_fov(75.0, AdjustDirection::Down), 70.0);
        assert_eq!(adjust_fov(118.0, AdjustDirection::Up), 120.0);
        assert_eq!(
            adjust_fov(120.0, AdjustDirection::Up),
            120.0,
            "pinned at MAX"
        );
        assert_eq!(adjust_fov(33.0, AdjustDirection::Down), 30.0);
        assert_eq!(
            adjust_fov(30.0, AdjustDirection::Down),
            30.0,
            "pinned at MIN"
        );
    }

    #[test]
    fn clamping_bounds_are_inclusive_and_match_the_load_path_constants() {
        assert_eq!(
            clamp_sensitivity(SENSITIVITY_MIN as f32),
            SENSITIVITY_MIN as f32
        );
        assert_eq!(
            clamp_sensitivity(SENSITIVITY_MAX as f32),
            SENSITIVITY_MAX as f32
        );
        assert_eq!(clamp_fov(FOV_MIN as f32), FOV_MIN as f32);
        assert_eq!(clamp_fov(FOV_MAX as f32), FOV_MAX as f32);
        // boundary values are accepted unchanged, not clamped away
        assert_eq!(clamp_fov(120.0), 120.0);
        assert_eq!(clamp_sensitivity(0.0001), 0.0001);
        // out-of-range values snap to the nearest bound
        assert_eq!(clamp_fov(500.0), 120.0);
        assert_eq!(clamp_sensitivity(0.5), SENSITIVITY_MAX as f32);
    }

    // --- rebind decisions ---

    #[test]
    fn rebind_to_a_free_supported_key_is_accepted() {
        let binds = Keybinds::default();
        // a free supported key: jump_or_up -> ArrowUp
        let decision = evaluate_rebind_candidate(&binds, BindAction::JumpOrUp, KeyCode::ArrowUp);
        assert_eq!(decision, RebindDecision::Accept(KeyCode::ArrowUp));
        // sprint -> KeyQ is also free
        let decision = evaluate_rebind_candidate(&binds, BindAction::Sprint, KeyCode::KeyQ);
        assert_eq!(decision, RebindDecision::Accept(KeyCode::KeyQ));
    }

    #[test]
    fn pressing_the_key_already_bound_to_the_same_action_is_an_idempotent_accept() {
        let binds = Keybinds::default();
        let decision = evaluate_rebind_candidate(&binds, BindAction::Forward, KeyCode::KeyW);
        assert_eq!(decision, RebindDecision::Accept(KeyCode::KeyW));
    }

    #[test]
    fn rebind_to_another_actions_key_is_rejected_as_duplicate() {
        let binds = Keybinds::default();
        // jump_or_up -> KeyW collides with forward
        let decision = evaluate_rebind_candidate(&binds, BindAction::JumpOrUp, KeyCode::KeyW);
        assert_eq!(
            decision,
            RebindDecision::RejectDuplicate {
                conflicting_action: BindAction::Forward
            }
        );
        // sprint -> Space collides with jump_or_up
        let decision = evaluate_rebind_candidate(&binds, BindAction::Sprint, KeyCode::Space);
        assert_eq!(
            decision,
            RebindDecision::RejectDuplicate {
                conflicting_action: BindAction::JumpOrUp
            }
        );
        // the slice demo's press-to-rebind case: jump_or_up -> ShiftLeft
        // collides with the default crouch binding
        let decision = evaluate_rebind_candidate(&binds, BindAction::JumpOrUp, KeyCode::ShiftLeft);
        assert_eq!(
            decision,
            RebindDecision::RejectDuplicate {
                conflicting_action: BindAction::CrouchOrDown
            }
        );
    }

    #[test]
    fn unsupported_keys_are_rejected_before_any_duplicate_check() {
        let binds = Keybinds::default();
        assert_eq!(
            evaluate_rebind_candidate(&binds, BindAction::Forward, KeyCode::Semicolon),
            RebindDecision::RejectUnsupported {
                key: KeyCode::Semicolon
            }
        );
        assert_eq!(
            evaluate_rebind_candidate(&binds, BindAction::Forward, KeyCode::Minus),
            RebindDecision::RejectUnsupported {
                key: KeyCode::Minus
            }
        );
    }

    #[test]
    fn accepted_rebinds_survive_the_settings_validate_keybinds_scan() {
        // The direct duplicate scan must agree with the authoritative
        // validator: apply each Accept and expect zero conflicts.
        let mut binds = Keybinds::default();
        let decision = evaluate_rebind_candidate(&binds, BindAction::JumpOrUp, KeyCode::ArrowUp);
        match decision {
            RebindDecision::Accept(code) => {
                BindAction::JumpOrUp.set_key(&mut binds, code);
            }
            other => panic!("expected accept, got {other:?}"),
        }
        let settings = crate::settings::Settings {
            keybinds: binds,
            ..Default::default()
        };
        assert!(
            settings.validate_keybinds().is_empty(),
            "an accepted rebind must not create a conflict"
        );
    }

    #[test]
    fn rebind_rejection_messages_name_the_culprit() {
        let binds = Keybinds::default();
        let dup = evaluate_rebind_candidate(&binds, BindAction::JumpOrUp, KeyCode::KeyW);
        let text = rebind_rejection_message(&dup).unwrap();
        assert!(text.contains("forward"), "message names the action: {text}");

        let unsup = evaluate_rebind_candidate(&binds, BindAction::Forward, KeyCode::Backslash);
        let text = rebind_rejection_message(&unsup).unwrap();
        assert!(text.contains("Backslash"), "message names the key: {text}");
        assert!(text.contains("supported"), "message says why: {text}");

        let acc = evaluate_rebind_candidate(&binds, BindAction::Sprint, KeyCode::KeyQ);
        assert_eq!(rebind_rejection_message(&acc), None);
    }

    // --- state transitions ---

    #[test]
    fn menu_opens_and_closes_and_reports_openness() {
        let mut menu = SettingsMenu::new();
        assert_eq!(menu.state, MenuState::Closed);
        assert!(!menu.is_open());
        assert!(menu.open(), "first open transitions");
        assert!(!menu.open(), "re-opening an open menu is a no-op false");
        assert!(menu.is_open());
        assert_eq!(menu.state, MenuState::Open);

        menu.close();
        assert_eq!(menu.state, MenuState::Closed);
        assert!(!menu.is_open());

        // close from capture mode also lands in Closed
        menu.open();
        menu.begin_rebind(BindAction::Sprint);
        menu.close();
        assert_eq!(menu.state, MenuState::Closed);
    }

    #[test]
    fn capture_mode_transitions_accept_and_cancel() {
        let mut menu = SettingsMenu::new();
        assert_eq!(menu.awaiting_action(), None);
        assert!(!menu.cancel_capture(), "nothing to cancel while closed");

        menu.open();
        menu.begin_rebind(BindAction::JumpOrUp);
        assert_eq!(
            menu.state,
            MenuState::AwaitingKey {
                action: BindAction::JumpOrUp
            }
        );
        assert_eq!(menu.awaiting_action(), Some(BindAction::JumpOrUp));
        assert!(menu.is_open(), "capture mode is still an open menu");

        // cancel: Escape during capture -> back to Open, menu stays open
        assert!(menu.cancel_capture());
        assert_eq!(menu.state, MenuState::Open);
        assert_eq!(menu.awaiting_action(), None);
        assert!(!menu.cancel_capture(), "second cancel is a no-op");

        // finish after an accepted rebind -> back to Open
        menu.begin_rebind(BindAction::Sprint);
        assert!(menu.finish_capture());
        assert_eq!(menu.state, MenuState::Open);
    }

    #[test]
    fn messages_expire_by_tick_counter_purely() {
        let mut menu = SettingsMenu::new();
        menu.set_message_for_ticks("dup", 2);
        assert_eq!(
            menu.message
                .as_ref()
                .map(|m| (m.text.as_str(), m.ticks_left)),
            Some(("dup", 2))
        );
        assert!(menu.tick());
        assert_eq!(menu.message.as_ref().unwrap().ticks_left, 1);
        assert!(!menu.tick(), "the expiring tick returns false");
        assert_eq!(menu.message, None, "expires at zero");
        assert!(!menu.tick(), "ticking with no message stays false");

        // a new message replaces the old one
        menu.set_message("second");
        assert_eq!(menu.message.as_ref().unwrap().ticks_left, MESSAGE_TICKS);
        assert_eq!(menu.message.as_ref().unwrap().text, "second");
        menu.clear_message();
        assert_eq!(menu.message, None);
    }

    // --- warnings formatting (MEM009: warnings are data) ---

    #[test]
    fn settings_warnings_format_as_field_reason_lines() {
        let warnings = vec![
            SettingsWarning {
                field: "video.fov".to_string(),
                reason: "expected a float, found string".to_string(),
            },
            SettingsWarning {
                field: "settings".to_string(),
                reason: "could not read settings file".to_string(),
            },
        ];
        let lines = format_warnings(&warnings);
        assert_eq!(
            lines,
            vec![
                "video.fov: expected a float, found string".to_string(),
                "settings: could not read settings file".to_string(),
            ]
        );
        assert!(format_warnings(&[]).is_empty());
    }

    // --- row model ---

    #[test]
    fn rows_cover_all_seven_binds_plus_settings_and_close_in_order() {
        assert_eq!(ROW_COUNT, 13);
        for (i, action) in BindAction::ALL.iter().enumerate() {
            assert_eq!(ROWS[i], MenuRow::Bind(*action), "row {i}");
        }
        assert_eq!(ROWS[7], MenuRow::MouseSensitivity);
        assert_eq!(ROWS[8], MenuRow::Fov);
        assert_eq!(ROWS[9], MenuRow::RenderDistance);
        assert_eq!(ROWS[10], MenuRow::Fullscreen);
        assert_eq!(ROWS[11], MenuRow::Vsync);
        assert_eq!(ROWS[12], MenuRow::Close);
        // every row has a non-empty label
        for r in ROWS {
            assert!(!r.label().is_empty());
        }
    }

    #[test]
    fn render_distance_steps_by_two_chunks_and_clamps_both_directions() {
        assert_eq!(adjust_render_distance(16, AdjustDirection::Up), 18);
        assert_eq!(adjust_render_distance(16, AdjustDirection::Down), 14);
        assert_eq!(
            adjust_render_distance(RENDER_DISTANCE_MAX - 1, AdjustDirection::Up),
            RENDER_DISTANCE_MAX,
            "clamps at the ceiling"
        );
        assert_eq!(
            adjust_render_distance(RENDER_DISTANCE_MAX, AdjustDirection::Up),
            RENDER_DISTANCE_MAX,
            "pinned at MAX"
        );
        assert_eq!(
            adjust_render_distance(RENDER_DISTANCE_MIN + 1, AdjustDirection::Down),
            RENDER_DISTANCE_MIN,
            "clamps at the floor"
        );
        assert_eq!(
            adjust_render_distance(RENDER_DISTANCE_MIN, AdjustDirection::Down),
            RENDER_DISTANCE_MIN,
            "pinned at MIN"
        );
        assert_eq!(clamp_render_distance(0), RENDER_DISTANCE_MIN);
        assert_eq!(clamp_render_distance(900), RENDER_DISTANCE_MAX);
    }

    #[test]
    fn bind_action_names_match_validate_keybinds_strings() {
        // the exact strings the settings file and conflict data use
        assert_eq!(BindAction::Forward.name(), "forward");
        assert_eq!(BindAction::Backward.name(), "backward");
        assert_eq!(BindAction::Left.name(), "left");
        assert_eq!(BindAction::Right.name(), "right");
        assert_eq!(BindAction::JumpOrUp.name(), "jump_or_up");
        assert_eq!(BindAction::CrouchOrDown.name(), "crouch_or_down");
        assert_eq!(BindAction::Sprint.name(), "sprint");
    }

    #[test]
    fn bind_action_get_set_round_trips_every_action() {
        let mut binds = Keybinds::default();
        for action in BindAction::ALL {
            action.set_key(&mut binds, KeyCode::KeyZ);
            assert_eq!(action.key_of(&binds), KeyCode::KeyZ);
        }
        // after binding everything to KeyZ, each action reads it back and
        // the authoritative validator reports conflicts naming them all
        let settings = crate::settings::Settings {
            keybinds: binds,
            ..Default::default()
        };
        let conflicts = settings.validate_keybinds();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].key, KeyCode::KeyZ);
        assert_eq!(conflicts[0].actions.len(), 7);
    }
}
