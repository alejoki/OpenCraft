//! Pure-logic lifecycle menu screens: row/target models, pixel-space
//! layout, hit-testing, new-game form state, load-screen catalog state,
//! delete-confirmation wrappers, modified-date formatting, and
//! screen-level keyboard routing.
//!
//! Same purity contract as `src/ui/menu.rs`: no GPU, no event loop, no
//! clock, no filesystem — every input is an argument and every output is
//! derived state, so every layout, validation, and confirmation decision
//! is unit-testable on CPU alone. The only outside types are data-only:
//! `winit`'s `KeyCode` (a plain enum, exactly like `menu.rs`), the
//! `TextInput` edit buffer, and the pure `world::catalog` contracts from
//! S01. `main.rs` (S02-T02/T03) owns the frame loop and applies these
//! helpers with `ctx.config.width/height` and `cursor_pos` each frame
//! (immediate mode), so a `Resized` reflows every screen automatically.
//! All coordinates are physical pixels, top-left origin, y down — the
//! same space as `Quad`, `ctx.config`, glyphon `TextArea`, and
//! `GameApp.cursor_pos` (P003/MEM012). Hit tests reuse `menu::hit_test`
//! (half-open like the pixel grid). Errors and transient messages are
//! data, never logged (D006/MEM009: the crate is log-free by convention).

use winit::keyboard::KeyCode;

use super::lifecycle::LifecycleScreen;
use super::menu::{hit_test, MenuMessage, Rect, MENU_PAD, MENU_TOP, MESSAGE_TICKS};
use super::text_input::{TextInput, TextInputAction, TextInputOutcome};
use super::LINE_HEIGHT;
use crate::world::catalog::{
    is_duplicate_name, validate_world_name, CatalogRow, ConfirmOutcome, DeleteConfirmation,
    PendingDelete, RowStatus, WorldNameError, MAX_WORLD_NAME_LEN,
};

/// Panel width in px for the main-menu, pause, and new-game screens.
/// Matches `menu::MENU_WIDTH` so all simple screens share one visual
/// width; wide enough for "Mouse Sensitivity: 0.0022"-style lines at
/// `FONT_SIZE` 18.
pub const SCREEN_WIDTH: f32 = 360.0;
/// Wider panel width in px for the load screen (name + date + Play and
/// Delete action targets on one row).
pub const LOAD_SCREEN_WIDTH: f32 = 520.0;
/// Width in px of each load-row action target (Play, Delete).
pub const ACTION_WIDTH: f32 = 96.0;
/// Maximum seed-field length in `char`s (a small, UI-friendly bound; the
/// seed is hashed via FNV-1a, so any length would work).
pub const SEED_MAX_CHARS: usize = 48;

// ---------------------------------------------------------------------------
// Row models: main menu and pause
// ---------------------------------------------------------------------------

/// One main-menu row, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainMenuRow {
    NewGame,
    LoadGame,
    Settings,
    ExitGame,
}

/// All main-menu rows, top-to-bottom in display order.
pub const MAIN_MENU_ROWS: [MainMenuRow; 4] = [
    MainMenuRow::NewGame,
    MainMenuRow::LoadGame,
    MainMenuRow::Settings,
    MainMenuRow::ExitGame,
];

/// Total number of main-menu rows.
pub const MAIN_MENU_ROW_COUNT: usize = MAIN_MENU_ROWS.len();

impl MainMenuRow {
    /// Human-facing row label.
    pub const fn label(self) -> &'static str {
        match self {
            MainMenuRow::NewGame => "New Game",
            MainMenuRow::LoadGame => "Load Game",
            MainMenuRow::Settings => "Settings",
            MainMenuRow::ExitGame => "Exit Game",
        }
    }
}

/// One pause-screen row, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseRow {
    Resume,
    Settings,
    SaveAndQuit,
}

/// All pause rows, top-to-bottom in display order.
pub const PAUSE_ROWS: [PauseRow; 3] = [PauseRow::Resume, PauseRow::Settings, PauseRow::SaveAndQuit];

/// Total number of pause rows.
pub const PAUSE_ROW_COUNT: usize = PAUSE_ROWS.len();

impl PauseRow {
    /// Human-facing row label.
    pub const fn label(self) -> &'static str {
        match self {
            PauseRow::Resume => "Resume",
            PauseRow::Settings => "Settings",
            PauseRow::SaveAndQuit => "Save and Quit",
        }
    }
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// Centered panel x for `width`, clamped at 0 on narrower surfaces —
/// the same convention as `menu::row_rect`, so a surface narrower than
/// the panel keeps the panel rooted to the left edge instead of
/// drifting offscreen negatively.
fn centered_x(surface_w: u32, width: f32) -> f32 {
    ((surface_w as f32 - width) / 2.0).max(0.0)
}

/// Top edge of row `index`: top-anchored under the stats overlay, same
/// as `menu::row_rect`.
fn row_y(index: usize) -> f32 {
    MENU_TOP + MENU_PAD + index as f32 * LINE_HEIGHT
}

/// Rect of row `index` in a fixed-width, fixed-count screen.
fn fixed_row_rect(surface_w: u32, index: usize, width: f32) -> Rect {
    (
        centered_x(surface_w, width),
        row_y(index),
        width,
        LINE_HEIGHT,
    )
}

/// Panel rect behind `row_count` fixed-width rows (rows plus
/// `MENU_PAD` padding on every side), same shape as `menu::panel_rect`.
fn fixed_panel_rect(surface_w: u32, width: f32, row_count: usize) -> Rect {
    (
        centered_x(surface_w, width) - MENU_PAD,
        MENU_TOP,
        width + 2.0 * MENU_PAD,
        2.0 * MENU_PAD + row_count as f32 * LINE_HEIGHT,
    )
}

/// Geometry of main-menu row `index`, or `None` out of range.
pub fn main_menu_row_rect(surface_w: u32, surface_h: u32, index: usize) -> Option<Rect> {
    let _ = surface_h;
    (index < MAIN_MENU_ROW_COUNT).then(|| fixed_row_rect(surface_w, index, SCREEN_WIDTH))
}

/// The dark panel rect behind all main-menu rows.
pub fn main_menu_panel_rect(surface_w: u32, surface_h: u32) -> Rect {
    let _ = surface_h;
    fixed_panel_rect(surface_w, SCREEN_WIDTH, MAIN_MENU_ROW_COUNT)
}

/// Which main-menu row the cursor is over, if any.
pub fn hovered_main_menu_row(
    cursor: (f32, f32),
    surface_w: u32,
    surface_h: u32,
) -> Option<MainMenuRow> {
    (0..MAIN_MENU_ROW_COUNT)
        .find(|&index| {
            main_menu_row_rect(surface_w, surface_h, index).is_some_and(|r| hit_test(cursor, r))
        })
        .map(|index| MAIN_MENU_ROWS[index])
}

/// Geometry of pause row `index`, or `None` out of range.
pub fn pause_row_rect(surface_w: u32, surface_h: u32, index: usize) -> Option<Rect> {
    let _ = surface_h;
    (index < PAUSE_ROW_COUNT).then(|| fixed_row_rect(surface_w, index, SCREEN_WIDTH))
}

/// The dark panel rect behind all pause rows.
pub fn pause_panel_rect(surface_w: u32, surface_h: u32) -> Rect {
    let _ = surface_h;
    fixed_panel_rect(surface_w, SCREEN_WIDTH, PAUSE_ROW_COUNT)
}

/// Which pause row the cursor is over, if any.
pub fn hovered_pause_row(cursor: (f32, f32), surface_w: u32, surface_h: u32) -> Option<PauseRow> {
    (0..PAUSE_ROW_COUNT)
        .find(|&index| {
            pause_row_rect(surface_w, surface_h, index).is_some_and(|r| hit_test(cursor, r))
        })
        .map(|index| PAUSE_ROWS[index])
}

// ---------------------------------------------------------------------------
// New-game screen: targets and geometry
// ---------------------------------------------------------------------------

/// Fixed season-length choices, in cycle order: game days per season.
/// The first entry is the roadmap-pinned default and is kept equal to
/// [`crate::world::season::DEFAULT_SEASON_LENGTH_DAYS`] so the row can
/// never disagree with the persistence default. Clicking (or selecting)
/// the row advances to the next choice and wraps, mirroring the settings
/// menu's `cycle_texture_pack` cycler — there is no parse/validation
/// path, so a bad value is unrepresentable.
pub const NEW_GAME_SEASON_LENGTHS: [u32; 4] =
    [crate::world::season::DEFAULT_SEASON_LENGTH_DAYS, 2, 4, 8];

/// One clickable/hoverable target on the new-game screen, in display
/// order: two text fields, then four action rows (the season-length row
/// is a cycler, like the settings-screen choice rows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewGameTarget {
    NameField,
    SeedField,
    Randomize,
    SeasonLength,
    Create,
    Back,
}

/// All new-game rows, top-to-bottom in display order.
pub const NEW_GAME_ROWS: [NewGameTarget; 6] = [
    NewGameTarget::NameField,
    NewGameTarget::SeedField,
    NewGameTarget::Randomize,
    NewGameTarget::SeasonLength,
    NewGameTarget::Create,
    NewGameTarget::Back,
];

/// Total number of new-game rows.
pub const NEW_GAME_ROW_COUNT: usize = NEW_GAME_ROWS.len();

impl NewGameTarget {
    /// Human-facing target label (field rows get their value appended by
    /// [`new_game_row_lines`]).
    pub const fn label(self) -> &'static str {
        match self {
            NewGameTarget::NameField => "World Name",
            NewGameTarget::SeedField => "Seed (blank = random)",
            NewGameTarget::Randomize => "Randomize Seed",
            NewGameTarget::SeasonLength => "Season Length",
            NewGameTarget::Create => "Create World",
            NewGameTarget::Back => "Back",
        }
    }
}

/// Geometry of new-game row `index`, or `None` out of range.
pub fn new_game_row_rect(surface_w: u32, surface_h: u32, index: usize) -> Option<Rect> {
    let _ = surface_h;
    (index < NEW_GAME_ROW_COUNT).then(|| fixed_row_rect(surface_w, index, SCREEN_WIDTH))
}

/// The dark panel rect behind all new-game rows.
pub fn new_game_panel_rect(surface_w: u32, surface_h: u32) -> Rect {
    let _ = surface_h;
    fixed_panel_rect(surface_w, SCREEN_WIDTH, NEW_GAME_ROW_COUNT)
}

/// Which new-game target the cursor is over, if any.
pub fn hovered_new_game_target(
    cursor: (f32, f32),
    surface_w: u32,
    surface_h: u32,
) -> Option<NewGameTarget> {
    (0..NEW_GAME_ROW_COUNT)
        .find(|&index| {
            new_game_row_rect(surface_w, surface_h, index).is_some_and(|r| hit_test(cursor, r))
        })
        .map(|index| NEW_GAME_ROWS[index])
}

// ---------------------------------------------------------------------------
// Load screen: targets and geometry
// ---------------------------------------------------------------------------

/// One clickable/hoverable target on the load screen. Catalog rows are
/// dynamic (`row_count` is an argument everywhere), and each row carries
/// Play and Delete sub-targets on its right side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadTarget {
    /// The text portion of catalog row `index` (hover highlight).
    Row(usize),
    /// The Play action of catalog row `index`.
    Play(usize),
    /// The Delete action of catalog row `index`.
    Delete(usize),
    /// The Back row below the catalog list.
    Back,
    /// The confirm row, shown while a delete is pending.
    ConfirmDelete,
    /// The cancel row, shown while a delete is pending.
    CancelDelete,
}

impl LoadTarget {
    /// Human-facing label, where a fixed one exists (row text is
    /// composed by [`format_row_line`]).
    pub const fn label(self) -> Option<&'static str> {
        match self {
            LoadTarget::Row(_) => None,
            LoadTarget::Play(_) => Some("Play"),
            LoadTarget::Delete(_) => Some("Delete"),
            LoadTarget::Back => Some("Back"),
            LoadTarget::ConfirmDelete => Some("Confirm Delete"),
            LoadTarget::CancelDelete => Some("Cancel"),
        }
    }
}

/// Geometry of catalog row `index` (the full row including its action
/// targets), or `None` when `index >= row_count`.
pub fn load_row_rect(
    surface_w: u32,
    surface_h: u32,
    index: usize,
    row_count: usize,
) -> Option<Rect> {
    let _ = surface_h;
    (index < row_count).then(|| fixed_row_rect(surface_w, index, LOAD_SCREEN_WIDTH))
}

/// The Play sub-target rect of catalog row `index` (right-aligned inside
/// the row, `ACTION_WIDTH` wide). Valid for `index < row_count`; the
/// caller checks the row exists.
pub fn load_play_rect(surface_w: u32, index: usize) -> Rect {
    let x = centered_x(surface_w, LOAD_SCREEN_WIDTH) + LOAD_SCREEN_WIDTH - 2.0 * ACTION_WIDTH;
    (x, row_y(index), ACTION_WIDTH, LINE_HEIGHT)
}

/// The Delete sub-target rect of catalog row `index` (right-aligned,
/// adjacent to Play). Valid for `index < row_count`.
pub fn load_delete_rect(surface_w: u32, index: usize) -> Rect {
    let x = centered_x(surface_w, LOAD_SCREEN_WIDTH) + LOAD_SCREEN_WIDTH - ACTION_WIDTH;
    (x, row_y(index), ACTION_WIDTH, LINE_HEIGHT)
}

/// The Back row rect: one row below the catalog list (index
/// `row_count`).
pub fn load_back_rect(surface_w: u32, row_count: usize) -> Rect {
    fixed_row_rect(surface_w, row_count, LOAD_SCREEN_WIDTH)
}

/// The confirm row rect (index `row_count + 1`). Geometry always
/// exists; the event loop only shows/hit-tests it while a delete is
/// pending.
pub fn load_confirm_rect(surface_w: u32, row_count: usize) -> Rect {
    fixed_row_rect(surface_w, row_count + 1, LOAD_SCREEN_WIDTH)
}

/// The cancel row rect (index `row_count + 2`). Geometry always exists;
/// shown only while a delete is pending.
pub fn load_cancel_rect(surface_w: u32, row_count: usize) -> Rect {
    fixed_row_rect(surface_w, row_count + 2, LOAD_SCREEN_WIDTH)
}

/// The dark panel rect behind the catalog rows plus the back, confirm,
/// and cancel slots.
pub fn load_panel_rect(surface_w: u32, row_count: usize) -> Rect {
    fixed_panel_rect(surface_w, LOAD_SCREEN_WIDTH, row_count + 3)
}

/// Which load-screen target the cursor is over, if any. Play and Delete
/// are checked before the row itself (they are sub-rects of the row, so
/// the half-open convention makes the boundary between them land in
/// Delete).
pub fn hovered_load_target(
    cursor: (f32, f32),
    surface_w: u32,
    surface_h: u32,
    row_count: usize,
) -> Option<LoadTarget> {
    for index in 0..row_count {
        if hit_test(cursor, load_play_rect(surface_w, index)) {
            return Some(LoadTarget::Play(index));
        }
        if hit_test(cursor, load_delete_rect(surface_w, index)) {
            return Some(LoadTarget::Delete(index));
        }
        if load_row_rect(surface_w, surface_h, index, row_count)
            .is_some_and(|r| hit_test(cursor, r))
        {
            return Some(LoadTarget::Row(index));
        }
    }
    if hit_test(cursor, load_back_rect(surface_w, row_count)) {
        return Some(LoadTarget::Back);
    }
    if hit_test(cursor, load_confirm_rect(surface_w, row_count)) {
        return Some(LoadTarget::ConfirmDelete);
    }
    if hit_test(cursor, load_cancel_rect(surface_w, row_count)) {
        return Some(LoadTarget::CancelDelete);
    }
    None
}

// ---------------------------------------------------------------------------
// New-game screen state
// ---------------------------------------------------------------------------

/// Which new-game field receives text input. Tab cycles via
/// [`FocusField::cycle`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusField {
    Name,
    Seed,
}

impl FocusField {
    /// Cycle to the other field (Tab).
    pub const fn cycle(self) -> Self {
        match self {
            FocusField::Name => FocusField::Seed,
            FocusField::Seed => FocusField::Name,
        }
    }
}

/// Typed validation failure for the new-game form. Wraps the S01
/// `world::catalog` contracts so the inline screen text comes from the
/// same data the create path will enforce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewGameFormError {
    /// The proposed name failed `catalog::validate_world_name`.
    InvalidName(WorldNameError),
    /// The proposed name collides with an existing catalog row
    /// (`catalog::is_duplicate_name`, case-insensitive).
    DuplicateName,
}

impl core::fmt::Display for NewGameFormError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            NewGameFormError::InvalidName(err) => write!(f, "{err}"),
            NewGameFormError::DuplicateName => {
                write!(f, "A world with that name already exists.")
            }
        }
    }
}

/// Validate a proposed create: the name must pass
/// `catalog::validate_world_name` and not duplicate any existing row.
/// Invalid names are checked first, so a bad name reports its exact
/// `WorldNameError` reason even when it is also a duplicate.
pub fn validate_create(name_value: &str, rows: &[CatalogRow]) -> Result<(), NewGameFormError> {
    if let Err(err) = validate_world_name(name_value) {
        return Err(NewGameFormError::InvalidName(err));
    }
    if is_duplicate_name(name_value, rows) {
        return Err(NewGameFormError::DuplicateName);
    }
    Ok(())
}

/// The new-game screen's mutable state: two bounded text fields, a focus
/// field, the season-length cycler index, and a transient message slot
/// with the same tick-counter
/// pattern as `menu::SettingsMenu` (the module owns no clock). Pure data
/// plus transition methods — no rendering, no events, no filesystem.
#[derive(Clone, Debug, PartialEq)]
pub struct NewGameScreen {
    name: TextInput,
    seed: TextInput,
    focus: FocusField,
    /// Index into [`NEW_GAME_SEASON_LENGTHS`] — the season-length row's
    /// cycler state. Always in range: only [`Self::cycle_season_length`]
    /// writes it, and it wraps modulo the list length.
    season_length_index: usize,
    message: Option<MenuMessage>,
}

impl Default for NewGameScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl NewGameScreen {
    /// Empty form, focus on the name field, no message.
    pub fn new() -> Self {
        Self {
            name: TextInput::new(MAX_WORLD_NAME_LEN),
            seed: TextInput::new(SEED_MAX_CHARS),
            focus: FocusField::Name,
            season_length_index: 0,
            message: None,
        }
    }

    pub fn name(&self) -> &TextInput {
        &self.name
    }

    pub fn seed(&self) -> &TextInput {
        &self.seed
    }

    pub fn focus(&self) -> FocusField {
        self.focus
    }

    /// Cycle the focused field (Tab) and return the new focus.
    pub fn cycle_focus(&mut self) -> FocusField {
        self.focus = self.focus.cycle();
        self.focus
    }

    /// Set the focused field directly (click-to-focus on a field row).
    /// Setting the already-focused field is a harmless no-op; focus stays
    /// a single field either way (one writer per event).
    pub fn set_focus(&mut self, field: FocusField) {
        self.focus = field;
    }

    /// Randomize the seed: clears the seed field, because a blank seed
    /// means "pick a random seed at creation"
    /// (`catalog::SeedSource::Random`). The name field is untouched.
    pub fn randomize(&mut self) {
        self.seed.clear();
    }

    /// The selected season length in game days per season. Default is
    /// [`NEW_GAME_SEASON_LENGTHS`]`[0]` (1 day, roadmap-pinned); the
    /// value feeds `session::new_session` at Create time.
    pub fn season_length_days(&self) -> u32 {
        NEW_GAME_SEASON_LENGTHS[self.season_length_index]
    }

    /// The row's cycler index (0-based into
    /// [`NEW_GAME_SEASON_LENGTHS`]) — exposed for tests that pin the
    /// wrap order without duplicating the value list.
    pub fn season_length_index(&self) -> usize {
        self.season_length_index
    }

    /// Advance the season-length row to the next choice, wrapping after
    /// the last (1 → 2 → 4 → 8 → 1). Returns the new value. Modelled on
    /// the settings-screen cyclers: no text editing, no invalid states.
    pub fn cycle_season_length(&mut self) -> u32 {
        self.season_length_index = (self.season_length_index + 1) % NEW_GAME_SEASON_LENGTHS.len();
        self.season_length_days()
    }

    /// The currently focused field, by reference.
    pub fn focused_input(&self) -> &TextInput {
        match self.focus {
            FocusField::Name => &self.name,
            FocusField::Seed => &self.seed,
        }
    }

    /// The currently focused field, mutably — the event loop routes
    /// printable-character and editing events here.
    pub fn focused_input_mut(&mut self) -> &mut TextInput {
        match self.focus {
            FocusField::Name => &mut self.name,
            FocusField::Seed => &mut self.seed,
        }
    }

    /// Apply an editing action to the focused field (the event loop's
    /// single text-routing entry point).
    pub fn apply(&mut self, action: TextInputAction) -> TextInputOutcome {
        self.focused_input_mut().apply(action)
    }

    /// Show `text` as a transient message for `MESSAGE_TICKS` ticks
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

    /// The current transient message, if any.
    pub fn message(&self) -> Option<&MenuMessage> {
        self.message.as_ref()
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

/// Insert a `|` caret at `cursor`'s character index within `value`
/// (the focused field renders its insertion cursor).
fn insert_caret(value: &str, cursor: usize) -> String {
    let mut out = String::with_capacity(value.len() + 1);
    for (index, character) in value.chars().enumerate() {
        if index == cursor {
            out.push('|');
        }
        out.push(character);
    }
    if cursor >= value.chars().count() {
        out.push('|');
    }
    out
}

/// Compose the new-game screen's display lines, top-to-bottom: the two
/// field rows with their current values, the action rows, and the
/// season-length cycler row with its current value. The focused field's
/// line is prefixed `> ` (every other line `  `) and shows a `|` caret
/// at its value's cursor index; an unfocused blank (or whitespace-only)
/// seed renders as "(random)", matching `catalog::SeedSource::Random`.
/// The season-length row never takes text focus (Tab only toggles
/// [`FocusField`]), so it renders as a plain `Season Length: N day(s)`
/// value line and the pluralization is honest: "1 day" but "4 days".
pub fn new_game_row_lines(screen: &NewGameScreen) -> Vec<String> {
    let name = screen.name().value();
    let seed = screen.seed().value();
    let name_line = if screen.focus() == FocusField::Name {
        format!(
            "> World Name: {}",
            insert_caret(name, screen.name().cursor())
        )
    } else {
        format!("  World Name: {name}")
    };
    let seed_line = if screen.focus() == FocusField::Seed {
        format!("> Seed: {}", insert_caret(seed, screen.seed().cursor()))
    } else if seed.trim().is_empty() {
        "  Seed: (random)".to_string()
    } else {
        format!("  Seed: {seed}")
    };
    vec![
        name_line,
        seed_line,
        NewGameTarget::Randomize.label().to_string(),
        season_length_line(screen.season_length_days()),
        NewGameTarget::Create.label().to_string(),
        NewGameTarget::Back.label().to_string(),
    ]
}

/// The season-length row's display text, e.g. "Season Length: 1 day".
fn season_length_line(days: u32) -> String {
    if days == 1 {
        "Season Length: 1 day".to_string()
    } else {
        format!("Season Length: {days} days")
    }
}

// ---------------------------------------------------------------------------
// Load screen state
// ---------------------------------------------------------------------------

/// The load-game screen's mutable state: the catalog rows (injected by
/// the caller — this module never touches the filesystem), the S01
/// [`DeleteConfirmation`] machine, and the index of the row whose
/// delete is pending. Deleting only ever happens through
/// [`LoadScreen::confirm_delete`], which requires a prior
/// [`LoadScreen::request_delete`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadScreen {
    rows: Vec<CatalogRow>,
    confirmation: DeleteConfirmation,
    pending_delete: Option<usize>,
}

impl LoadScreen {
    /// An empty screen (no rows, no pending delete).
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            confirmation: DeleteConfirmation::new(),
            pending_delete: None,
        }
    }

    /// Replace the catalog rows (entering or refreshing the screen).
    /// Any pending delete is cancelled: after a refresh the old row
    /// identity may be stale, and a delete must never fire from stale
    /// confirmation state.
    pub fn set_rows(&mut self, rows: Vec<CatalogRow>) {
        self.rows = rows;
        self.cancel_delete();
    }

    /// The catalog rows, newest-first as injected by the caller.
    pub fn rows(&self) -> &[CatalogRow] {
        &self.rows
    }

    /// Number of catalog rows (drives the load-screen geometry).
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Is a delete awaiting confirmation?
    pub fn is_delete_pending(&self) -> bool {
        self.confirmation.is_pending()
    }

    /// The index of the row whose delete is pending, if any.
    pub fn pending_row(&self) -> Option<usize> {
        self.pending_delete
    }

    /// Request delete confirmation for row `index`. Out-of-range
    /// indices are ignored (returns `false`); the pending identity is
    /// kept alongside the S01 confirmation machine so the prompt and
    /// row highlight stay consistent. A new request replaces any
    /// previous one.
    pub fn request_delete(&mut self, index: usize) -> bool {
        let Some(row) = self.rows.get(index) else {
            return false;
        };
        self.confirmation.request(row);
        self.pending_delete = Some(index);
        true
    }

    /// Confirm the pending delete: returns the S01 [`PendingDelete`]
    /// (the file the caller should actually delete) and clears the
    /// pending state, or `None` when nothing was pending — a delete can
    /// never fire without a prior request.
    pub fn confirm_delete(&mut self) -> Option<PendingDelete> {
        self.pending_delete = None;
        match self.confirmation.confirm() {
            ConfirmOutcome::Confirmed(pending) => Some(pending),
            ConfirmOutcome::NothingPending => None,
        }
    }

    /// Cancel the pending delete (cancel target, or Escape before
    /// backing out). Clears both the confirmation machine and the
    /// pending row identity.
    pub fn cancel_delete(&mut self) {
        self.confirmation.cancel();
        self.pending_delete = None;
    }
}

// ---------------------------------------------------------------------------
// Load screen text composition
// ---------------------------------------------------------------------------

/// Convert days since the Unix epoch to a civil `(year, month, day)`
/// date. Howard Hinnant's `civil_from_days` algorithm (public domain):
/// exact for the proleptic Gregorian calendar, no `chrono` dependency.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

/// Render a modified timestamp (seconds since the Unix epoch) as a
/// `YYYY-MM-DD` date, or `"unknown"` when the timestamp could not be
/// read. Pure and total: any `u64` renders without panicking.
pub fn format_modified_secs(secs: Option<u64>) -> String {
    match secs {
        None => "unknown".to_string(),
        Some(secs) => {
            let days = (secs / 86_400) as i64;
            let (year, month, day) = civil_from_days(days);
            format!("{year:04}-{month:02}-{day:02}")
        }
    }
}

/// Compose one catalog row's display line: "name — date", with corrupt
/// rows additionally marked non-loadable ("— (cannot load)") so the
/// cleanup path (delete) stays visible (MEM009: the row is degraded,
/// not excluded).
pub fn format_row_line(row: &CatalogRow) -> String {
    let date = format_modified_secs(row.modified_secs);
    match row.status {
        RowStatus::Loadable => format!("{} — {}", row.name, date),
        RowStatus::Corrupt(_) => format!("{} — {} — (cannot load)", row.name, date),
    }
}

/// The delete-confirmation prompt line naming the pending row.
pub fn delete_prompt_line(name: &str) -> String {
    format!("Delete '{name}'?")
}

// ---------------------------------------------------------------------------
// Screen-level keyboard routing
// ---------------------------------------------------------------------------

/// What a screen-level key press means, as a small vocabulary the event
/// loop applies — mirroring how `text_input::TextInputAction`
/// decouples winit from edit semantics. Screen-specific keys only;
/// printable-character routing into the focused field is the event
/// loop's job via [`NewGameScreen::apply`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenKeyAction {
    /// The key has no screen-level meaning.
    None,
    /// Navigate back one screen. On the pause screen the caller applies
    /// this as Resume (the non-destructive default); on the main menu
    /// there is nothing to back out to, so the caller treats Escape as
    /// a documented no-op (the simplest non-destructive behavior).
    Back,
    /// Escape while a delete is pending: cancel it before backing out.
    CancelPendingDelete,
    /// Enter while a delete is pending: confirm it.
    ConfirmPendingDelete,
    /// Enter on the new-game screen: attempt to create (the caller runs
    /// [`validate_create`] and shows any error inline).
    Create,
    /// Tab on the new-game screen: cycle the focused field.
    CycleField,
}

/// Map a screen-level key press to its action for the current screen.
///
/// Per-screen semantics (pure total function over the S01
/// [`LifecycleScreen`] data):
/// - **NewGame**: Escape → Back, Enter/NumpadEnter → Create, Tab →
///   CycleField.
/// - **LoadGame**: Escape cancels a pending delete first
///   (`delete_pending`), otherwise backs out; Enter confirms a pending
///   delete, otherwise has no screen-level meaning.
/// - **MainMenu**: nothing — the main menu is the root screen, so
///   Escape is a documented no-op at the caller.
/// - **Pause**: Escape → Back (the caller applies it as Resume).
/// - **Playing / Settings**: nothing — gameplay keys and the settings
///   menu's own handler own those.
pub fn screen_key_action(
    screen: LifecycleScreen,
    key: KeyCode,
    delete_pending: bool,
) -> ScreenKeyAction {
    let enter = matches!(key, KeyCode::Enter | KeyCode::NumpadEnter);
    match screen {
        LifecycleScreen::NewGame => match key {
            KeyCode::Escape => ScreenKeyAction::Back,
            KeyCode::Tab => ScreenKeyAction::CycleField,
            _ if enter => ScreenKeyAction::Create,
            _ => ScreenKeyAction::None,
        },
        LifecycleScreen::LoadGame => match key {
            KeyCode::Escape => {
                if delete_pending {
                    ScreenKeyAction::CancelPendingDelete
                } else {
                    ScreenKeyAction::Back
                }
            }
            _ if enter => {
                if delete_pending {
                    ScreenKeyAction::ConfirmPendingDelete
                } else {
                    ScreenKeyAction::None
                }
            }
            _ => ScreenKeyAction::None,
        },
        LifecycleScreen::Pause => match key {
            KeyCode::Escape => ScreenKeyAction::Back,
            _ => ScreenKeyAction::None,
        },
        LifecycleScreen::MainMenu | LifecycleScreen::Playing | LifecycleScreen::Settings(_) => {
            ScreenKeyAction::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::catalog::{resolve_seed, RowProblem, SeedSource};

    const SW: u32 = 1280;
    const SH: u32 = 720;

    // --- row models ---

    #[test]
    fn main_menu_rows_are_in_display_order_with_labels() {
        assert_eq!(MAIN_MENU_ROW_COUNT, 4);
        assert_eq!(
            MAIN_MENU_ROWS,
            [
                MainMenuRow::NewGame,
                MainMenuRow::LoadGame,
                MainMenuRow::Settings,
                MainMenuRow::ExitGame
            ]
        );
        assert_eq!(MainMenuRow::NewGame.label(), "New Game");
        assert_eq!(MainMenuRow::LoadGame.label(), "Load Game");
        assert_eq!(MainMenuRow::Settings.label(), "Settings");
        assert_eq!(MainMenuRow::ExitGame.label(), "Exit Game");
    }

    #[test]
    fn pause_rows_are_in_display_order_with_labels() {
        assert_eq!(PAUSE_ROW_COUNT, 3);
        assert_eq!(
            PAUSE_ROWS,
            [PauseRow::Resume, PauseRow::Settings, PauseRow::SaveAndQuit]
        );
        assert_eq!(PauseRow::Resume.label(), "Resume");
        assert_eq!(PauseRow::Settings.label(), "Settings");
        assert_eq!(PauseRow::SaveAndQuit.label(), "Save and Quit");
    }

    #[test]
    fn every_row_and_target_label_is_non_empty() {
        for row in MAIN_MENU_ROWS {
            assert!(!row.label().is_empty());
        }
        for row in PAUSE_ROWS {
            assert!(!row.label().is_empty());
        }
        for target in NEW_GAME_ROWS {
            assert!(!target.label().is_empty());
        }
        for target in [
            LoadTarget::Play(0),
            LoadTarget::Delete(0),
            LoadTarget::Back,
            LoadTarget::ConfirmDelete,
            LoadTarget::CancelDelete,
        ] {
            assert!(!target.label().unwrap().is_empty());
        }
        assert!(
            LoadTarget::Row(0).label().is_none(),
            "rows compose their own text"
        );
    }

    // --- layout ---

    #[test]
    fn fixed_screens_stack_centered_line_height_rows_deterministically() {
        for (rect_at, count, width) in [
            (
                main_menu_row_rect as fn(u32, u32, usize) -> Option<Rect>,
                MAIN_MENU_ROW_COUNT,
                SCREEN_WIDTH,
            ),
            (
                pause_row_rect as fn(u32, u32, usize) -> Option<Rect>,
                PAUSE_ROW_COUNT,
                SCREEN_WIDTH,
            ),
            (
                new_game_row_rect as fn(u32, u32, usize) -> Option<Rect>,
                NEW_GAME_ROW_COUNT,
                SCREEN_WIDTH,
            ),
        ] {
            let (x0, y0, w0, h0) = rect_at(SW, SH, 0).unwrap();
            assert_eq!(x0, (SW as f32 - width) / 2.0, "rows are centered");
            assert_eq!(y0, MENU_TOP + MENU_PAD);
            assert_eq!(w0, width);
            assert_eq!(h0, LINE_HEIGHT);
            for index in 0..count {
                assert_eq!(
                    rect_at(SW, SH, index),
                    rect_at(SW, SH, index),
                    "row {index} rect must be stable across calls"
                );
                let (x, y, w, h) = rect_at(SW, SH, index).unwrap();
                assert_eq!(x, x0, "all rows share the panel x");
                assert_eq!(w, width);
                assert_eq!(h, LINE_HEIGHT);
                assert_eq!(y, y0 + index as f32 * LINE_HEIGHT);
            }
            assert_eq!(rect_at(SW, SH, count), None, "past the last row");
        }
    }

    #[test]
    fn load_geometry_stacks_rows_then_back_confirm_cancel() {
        let row_count = 2;
        // rows exist below the count and not past it
        for index in 0..row_count {
            let (x, y, w, h) = load_row_rect(SW, SH, index, row_count).unwrap();
            assert_eq!(x, (SW as f32 - LOAD_SCREEN_WIDTH) / 2.0);
            assert_eq!(w, LOAD_SCREEN_WIDTH);
            assert_eq!(h, LINE_HEIGHT);
            assert_eq!(y, MENU_TOP + MENU_PAD + index as f32 * LINE_HEIGHT);
        }
        assert_eq!(load_row_rect(SW, SH, row_count, row_count), None);

        // back, confirm, cancel follow the rows in order, sharing geometry
        let (bx, by, bw, bh) = load_back_rect(SW, row_count);
        let (cx, cy, cw, ch) = load_confirm_rect(SW, row_count);
        let (x_x, x_y, w_x, h_x) = load_cancel_rect(SW, row_count);
        assert_eq!(
            (bx, bw, bh),
            (
                (SW as f32 - LOAD_SCREEN_WIDTH) / 2.0,
                LOAD_SCREEN_WIDTH,
                LINE_HEIGHT
            )
        );
        assert_eq!(by, MENU_TOP + MENU_PAD + row_count as f32 * LINE_HEIGHT);
        assert_eq!((cx, cw, ch), (bx, bw, bh));
        assert_eq!(cy, by + LINE_HEIGHT);
        assert_eq!((x_x, w_x, h_x), (bx, bw, bh));
        assert_eq!(x_y, cy + LINE_HEIGHT);
    }

    #[test]
    fn load_geometry_with_an_empty_rows_list_still_lays_out_back_and_actions() {
        // empty catalog: no rows, back at index 0, confirm/cancel after
        assert_eq!(load_row_rect(SW, SH, 0, 0), None);
        let (_, by, ..) = load_back_rect(SW, 0);
        assert_eq!(by, MENU_TOP + MENU_PAD);
        let (_, cy, ..) = load_confirm_rect(SW, 0);
        assert_eq!(cy, by + LINE_HEIGHT);
        let (_, xy, ..) = load_cancel_rect(SW, 0);
        assert_eq!(xy, cy + LINE_HEIGHT);
    }

    #[test]
    fn panels_cover_every_row_plus_symmetric_padding() {
        for (panel, rect_at, count, width) in [
            (
                main_menu_panel_rect as fn(u32, u32) -> Rect,
                main_menu_row_rect as fn(u32, u32, usize) -> Option<Rect>,
                MAIN_MENU_ROW_COUNT,
                SCREEN_WIDTH,
            ),
            (
                pause_panel_rect as fn(u32, u32) -> Rect,
                pause_row_rect as fn(u32, u32, usize) -> Option<Rect>,
                PAUSE_ROW_COUNT,
                SCREEN_WIDTH,
            ),
            (
                new_game_panel_rect as fn(u32, u32) -> Rect,
                new_game_row_rect as fn(u32, u32, usize) -> Option<Rect>,
                NEW_GAME_ROW_COUNT,
                SCREEN_WIDTH,
            ),
        ] {
            let (px, py, pw, ph) = panel(SW, SH);
            for index in 0..count {
                let (x, y, w, h) = rect_at(SW, SH, index).unwrap();
                assert!(
                    px <= x && x + w <= px + pw,
                    "row {index} inside panel horizontally"
                );
                assert!(
                    py <= y && y + h <= py + ph,
                    "row {index} inside panel vertically"
                );
            }
            let (x0, y0, ..) = rect_at(SW, SH, 0).unwrap();
            let (_, _, _, h_last) = rect_at(SW, SH, count - 1).unwrap();
            assert_eq!(x0 - px, MENU_PAD);
            assert_eq!(y0 - py, MENU_PAD);
            let last_bottom = y0 + (count - 1) as f32 * LINE_HEIGHT + h_last;
            assert_eq!(py + ph - last_bottom, MENU_PAD);
            assert_eq!(pw, width + 2.0 * MENU_PAD);
        }

        // load panel: rows + back + confirm + cancel, symmetric padding
        for row_count in [0usize, 1, 5] {
            let (px, py, pw, ph) = load_panel_rect(SW, row_count);
            let rects = (0..row_count)
                .map(|index| load_row_rect(SW, SH, index, row_count).unwrap())
                .chain([
                    load_back_rect(SW, row_count),
                    load_confirm_rect(SW, row_count),
                    load_cancel_rect(SW, row_count),
                ]);
            for (i, (x, y, w, h)) in rects.enumerate() {
                assert!(
                    px <= x && x + w <= px + pw,
                    "slot {i} inside panel horizontally"
                );
                assert!(
                    py <= y && y + h <= py + ph,
                    "slot {i} inside panel vertically"
                );
            }
            let (_, first_y, ..) = if row_count > 0 {
                load_row_rect(SW, SH, 0, row_count).unwrap()
            } else {
                load_back_rect(SW, row_count)
            };
            let (_, cancel_y, _, cancel_h) = load_cancel_rect(SW, row_count);
            assert_eq!(first_y - py, MENU_PAD);
            assert_eq!(py + ph - (cancel_y + cancel_h), MENU_PAD);
            assert_eq!(pw, LOAD_SCREEN_WIDTH + 2.0 * MENU_PAD);
        }
    }

    #[test]
    fn narrow_and_zero_surfaces_clamp_the_panel_x_at_zero() {
        // every screen: narrower than the panel -> x = 0, never negative
        assert_eq!(main_menu_row_rect(200, SH, 0).unwrap().0, 0.0);
        assert_eq!(pause_row_rect(200, SH, 0).unwrap().0, 0.0);
        assert_eq!(new_game_row_rect(200, SH, 0).unwrap().0, 0.0);
        assert_eq!(load_row_rect(200, SH, 0, 3).unwrap().0, 0.0);
        // zero-width surface
        assert_eq!(main_menu_row_rect(0, SH, 0).unwrap().0, 0.0);
        assert_eq!(load_row_rect(0, SH, 0, 3).unwrap().0, 0.0);
        // panel follows the same clamp
        assert_eq!(main_menu_panel_rect(200, SH).0, 0.0 - MENU_PAD);
        assert_eq!(load_panel_rect(200, 3).0, 0.0 - MENU_PAD);
    }

    #[test]
    fn play_and_delete_subtargets_are_inside_the_row_adjacent_and_disjoint() {
        for index in 0..3 {
            let (rx, ry, rw, rh) = load_row_rect(SW, SH, index, 3).unwrap();
            let (px, py, pw, ph) = load_play_rect(SW, index);
            let (dx, dy, dw, dh) = load_delete_rect(SW, index);
            assert_eq!((py, ph), (ry, rh));
            assert_eq!((dy, dh), (ry, rh));
            assert_eq!(px + pw, dx, "Play and Delete are adjacent");
            assert_eq!(dx + dw, rx + rw, "Delete ends at the row's right edge");
            assert!(rx <= px && dx + dw <= rx + rw, "targets inside the row");
            // half-open disjoint: a point on the shared edge lands in Delete
            assert_eq!(
                hovered_load_target((px + pw, py + 1.0), SW, SH, 3),
                Some(LoadTarget::Delete(index)),
                "the Play/Delete boundary belongs to Delete"
            );
        }
    }

    // --- hit-testing ---

    #[test]
    fn hovered_targets_find_the_target_under_the_cursor_and_none_elsewhere() {
        // main menu row 2
        let (x, y0, w, h) = main_menu_row_rect(SW, SH, 2).unwrap();
        assert_eq!(
            hovered_main_menu_row((x + w / 2.0, y0 + h / 2.0), SW, SH),
            Some(MainMenuRow::Settings)
        );
        assert_eq!(
            hovered_main_menu_row((0.0, 0.0), SW, SH),
            None,
            "above the panel"
        );
        assert_eq!(
            hovered_main_menu_row((SW as f32 - 1.0, SH as f32 - 1.0), SW, SH),
            None,
            "below the last row"
        );

        // pause row 1
        let (x, y0, w, h) = pause_row_rect(SW, SH, 1).unwrap();
        assert_eq!(
            hovered_pause_row((x + w / 2.0, y0 + h / 2.0), SW, SH),
            Some(PauseRow::Settings)
        );
        assert_eq!(hovered_pause_row((x, 0.0), SW, SH), None, "above the panel");

        // new-game targets: name field (0) and create (4; SeasonLength
        // was inserted at 3, shifting the action rows down)
        let (x, y0, w, h) = new_game_row_rect(SW, SH, 0).unwrap();
        assert_eq!(
            hovered_new_game_target((x + w / 2.0, y0 + h / 2.0), SW, SH),
            Some(NewGameTarget::NameField)
        );
        let (_, y3, w3, h3) = new_game_row_rect(SW, SH, 3).unwrap();
        assert_eq!(
            hovered_new_game_target((x + w3 / 2.0, y3 + h3 / 2.0), SW, SH),
            Some(NewGameTarget::SeasonLength)
        );
        let (_, y4, w4, h4) = new_game_row_rect(SW, SH, 4).unwrap();
        assert_eq!(
            hovered_new_game_target((x + w4 / 2.0, y4 + h4 / 2.0), SW, SH),
            Some(NewGameTarget::Create)
        );
        assert_eq!(hovered_new_game_target((0.0, 0.0), SW, SH), None);
    }

    #[test]
    fn load_hover_finds_play_delete_row_back_confirm_and_cancel() {
        let row_count = 2;
        let (x, y0, w, h) = load_row_rect(SW, SH, 1, row_count).unwrap();
        let mid_y = y0 + h / 2.0;
        // left text portion -> Row(1)
        assert_eq!(
            hovered_load_target((x + 10.0, mid_y), SW, SH, row_count),
            Some(LoadTarget::Row(1))
        );
        // Play sub-target of row 1
        let (px, ..) = load_play_rect(SW, 1);
        assert_eq!(
            hovered_load_target((px + 5.0, mid_y), SW, SH, row_count),
            Some(LoadTarget::Play(1))
        );
        // Delete sub-target of row 0
        let (dx, dy, ..) = load_delete_rect(SW, 0);
        assert_eq!(
            hovered_load_target((dx + 5.0, dy + 1.0), SW, SH, row_count),
            Some(LoadTarget::Delete(0))
        );
        // Back, Confirm, Cancel
        let (bx, by, bw, bh) = load_back_rect(SW, row_count);
        assert_eq!(
            hovered_load_target((bx + bw / 2.0, by + bh / 2.0), SW, SH, row_count),
            Some(LoadTarget::Back)
        );
        let (cx, cy, cw, ch) = load_confirm_rect(SW, row_count);
        assert_eq!(
            hovered_load_target((cx + cw / 2.0, cy + ch / 2.0), SW, SH, row_count),
            Some(LoadTarget::ConfirmDelete)
        );
        let (xx, xy, xw, xh) = load_cancel_rect(SW, row_count);
        assert_eq!(
            hovered_load_target((xx + xw / 2.0, xy + xh / 2.0), SW, SH, row_count),
            Some(LoadTarget::CancelDelete)
        );
        // misses
        assert_eq!(hovered_load_target((0.0, 0.0), SW, SH, row_count), None);
        assert_eq!(
            hovered_load_target((SW as f32 - 1.0, SH as f32 - 1.0), SW, SH, row_count),
            None,
            "below the cancel row"
        );
        // empty catalog: no row targets, back still reachable
        assert_eq!(
            hovered_load_target((x + w / 2.0, load_back_rect(SW, 0).1 + 1.0), SW, SH, 0),
            Some(LoadTarget::Back)
        );
    }

    #[test]
    fn hit_tests_are_half_open_like_the_pixel_grid() {
        // left/top edges inside, right/bottom edges outside — via the
        // shared menu::hit_test convention, exercised through the
        // hovered helpers.
        let (x, y, w, h) = main_menu_row_rect(SW, SH, 0).unwrap();
        assert_eq!(
            hovered_main_menu_row((x, y), SW, SH),
            Some(MainMenuRow::NewGame),
            "top-left corner is inside"
        );
        assert_eq!(
            hovered_main_menu_row((x + w, y + 1.0), SW, SH),
            None,
            "right edge of the last-at-that-y row is outside"
        );
        // the bottom edge of row i is the top edge of row i+1 (stacked)
        let (x1, ..) = main_menu_row_rect(SW, SH, 1).unwrap();
        assert_eq!(
            hovered_main_menu_row((x1, y + h), SW, SH),
            Some(MAIN_MENU_ROWS[1]),
            "the bottom edge of row 0 is the top edge of row 1"
        );
        // and past the last row's bottom edge: nothing
        let last = MAIN_MENU_ROW_COUNT - 1;
        let (xl, yl, _, hl) = main_menu_row_rect(SW, SH, last).unwrap();
        assert_eq!(
            hovered_main_menu_row((xl, yl + hl), SW, SH),
            None,
            "the last row's bottom edge is outside the panel"
        );
    }

    // --- new-game form state ---

    #[test]
    fn focus_defaults_to_name_and_cycles_between_the_two_fields() {
        let mut screen = NewGameScreen::new();
        assert_eq!(screen.focus(), FocusField::Name);
        assert_eq!(screen.cycle_focus(), FocusField::Seed);
        assert_eq!(screen.focus(), FocusField::Seed);
        assert_eq!(screen.cycle_focus(), FocusField::Name);
        assert_eq!(screen.focus(), FocusField::Name);
        assert_eq!(FocusField::Name.cycle(), FocusField::Seed);
        assert_eq!(FocusField::Seed.cycle(), FocusField::Name);

        // set_focus drives the same single field (click-to-focus), and
        // set + cycle interleave consistently with cycle-only routing
        screen.set_focus(FocusField::Seed);
        assert_eq!(screen.focus(), FocusField::Seed);
        assert_eq!(screen.cycle_focus(), FocusField::Name);
        screen.set_focus(FocusField::Name);
        assert_eq!(screen.focus(), FocusField::Name);
        assert_eq!(screen.cycle_focus(), FocusField::Seed);
        screen.set_focus(FocusField::Seed);
        assert_eq!(screen.focus(), FocusField::Seed);
        // setting the already-focused field is a harmless no-op
        screen.set_focus(FocusField::Seed);
        assert_eq!(screen.focus(), FocusField::Seed);
        // text still routes to whichever field set_focus chose
        screen.apply(TextInputAction::InsertText("via click".into()));
        assert_eq!(screen.seed().value(), "via click");
        assert_eq!(screen.name().value(), "");
    }

    #[test]
    fn text_routing_goes_to_the_focused_field_within_its_bound() {
        let mut screen = NewGameScreen::new();
        // name field: bounded by MAX_WORLD_NAME_LEN
        screen.apply(TextInputAction::InsertText("My World".into()));
        assert_eq!(screen.name().value(), "My World");
        assert_eq!(screen.seed().value(), "");
        let over_long = "a".repeat(MAX_WORLD_NAME_LEN + 5);
        screen.apply(TextInputAction::InsertText(over_long));
        assert_eq!(
            screen.name().len(),
            MAX_WORLD_NAME_LEN,
            "the name field is bounded"
        );

        // cycle to seed: bounded by SEED_MAX_CHARS
        screen.cycle_focus();
        screen.apply(TextInputAction::InsertText("glacier 7".into()));
        assert_eq!(screen.seed().value(), "glacier 7");
        assert_eq!(
            screen.name().value().chars().take(8).collect::<String>(),
            "My World"
        );
        let long_seed = "s".repeat(SEED_MAX_CHARS + 5);
        screen.apply(TextInputAction::InsertText(long_seed));
        assert_eq!(
            screen.seed().len(),
            SEED_MAX_CHARS,
            "the seed field is bounded"
        );
    }

    #[test]
    fn randomize_clears_the_seed_only_and_blank_means_random() {
        let mut screen = NewGameScreen::new();
        screen.apply(TextInputAction::InsertText("kept".into()));
        screen.cycle_focus();
        screen.apply(TextInputAction::InsertText("chosen seed".into()));

        screen.randomize();
        assert_eq!(screen.seed().value(), "", "randomize clears the seed field");
        assert_eq!(screen.name().value(), "kept", "the name is untouched");
        // the cleared seed resolves to SeedSource::Random, matching the
        // create path's contract
        assert_eq!(resolve_seed(screen.seed().value()), SeedSource::Random);
        // randomize on an already-blank field is a harmless no-op
        screen.randomize();
        assert_eq!(screen.seed().value(), "");
    }

    #[test]
    fn new_game_messages_expire_by_tick_counter_purely() {
        let mut screen = NewGameScreen::new();
        assert!(screen.message().is_none());
        assert!(!screen.tick(), "ticking with no message stays false");

        screen.set_message_for_ticks("oops", 2);
        assert_eq!(
            screen.message().map(|m| (m.text.as_str(), m.ticks_left)),
            Some(("oops", 2))
        );
        assert!(screen.tick());
        assert_eq!(screen.message().unwrap().ticks_left, 1);
        assert!(!screen.tick(), "the expiring tick returns false");
        assert!(screen.message().is_none(), "expires at zero");

        // a new message replaces the old one with the default lifetime
        screen.set_message("second");
        assert_eq!(screen.message().unwrap().ticks_left, MESSAGE_TICKS);
        assert_eq!(screen.message().unwrap().text, "second");
        screen.clear_message();
        assert!(screen.message().is_none());
    }

    // --- validate_create ---

    #[test]
    fn validate_create_rejects_every_world_name_error_variant() {
        let rows = vec![CatalogRow::loadable(
            "Existing".into(),
            "Existing.mcvx".into(),
            Some(10),
        )];
        // Empty (and whitespace-only)
        assert_eq!(
            validate_create("", &rows),
            Err(NewGameFormError::InvalidName(WorldNameError::Empty))
        );
        assert_eq!(
            validate_create("   ", &rows),
            Err(NewGameFormError::InvalidName(WorldNameError::Empty))
        );
        // TooLong
        let too_long = "a".repeat(MAX_WORLD_NAME_LEN + 1);
        assert_eq!(
            validate_create(&too_long, &rows),
            Err(NewGameFormError::InvalidName(WorldNameError::TooLong {
                len: MAX_WORLD_NAME_LEN + 1
            }))
        );
        // InvalidCharacter (path separator) at its exact index
        assert_eq!(
            validate_create("bad/name", &rows),
            Err(NewGameFormError::InvalidName(
                WorldNameError::InvalidCharacter { ch: '/', index: 3 }
            ))
        );
    }

    #[test]
    fn validate_create_rejects_duplicates_case_insensitively() {
        let rows = vec![
            CatalogRow::loadable("Overworld".into(), "Overworld.mcvx".into(), Some(10)),
            CatalogRow::loadable("Second".into(), "second.mcvx".into(), Some(20)),
        ];
        assert_eq!(
            validate_create("Overworld", &rows),
            Err(NewGameFormError::DuplicateName)
        );
        assert_eq!(
            validate_create(" overworld ", &rows),
            Err(NewGameFormError::DuplicateName)
        );
        assert_eq!(
            validate_create("SECOND", &rows),
            Err(NewGameFormError::DuplicateName)
        );
        // a fresh valid name passes
        assert_eq!(validate_create("Fresh World", &rows), Ok(()));
        // no rows at all: any valid name passes
        assert_eq!(validate_create("Fresh World", &[]), Ok(()));
    }

    #[test]
    fn form_errors_display_as_inline_screen_text() {
        let empty = NewGameFormError::InvalidName(WorldNameError::Empty);
        assert_eq!(empty.to_string(), "World name cannot be empty.");
        let dup = NewGameFormError::DuplicateName;
        assert_eq!(dup.to_string(), "A world with that name already exists.");
        let bad_char =
            NewGameFormError::InvalidName(WorldNameError::InvalidCharacter { ch: '/', index: 3 });
        assert!(bad_char.to_string().contains('/'), "names the character");
    }

    // --- load screen state ---

    fn two_rows() -> Vec<CatalogRow> {
        vec![
            CatalogRow::loadable("alpha".into(), "alpha.mcvx".into(), Some(100)),
            CatalogRow::corrupt(
                "broken".into(),
                "broken.mcvx".into(),
                None,
                RowProblem::InvalidFormat,
            ),
        ]
    }

    #[test]
    fn delete_confirm_requires_a_prior_request_and_is_single_use() {
        let mut screen = LoadScreen::new();
        screen.set_rows(two_rows());
        assert!(!screen.is_delete_pending());
        assert_eq!(screen.pending_row(), None);
        // confirm with nothing pending never fires a delete
        assert!(screen.confirm_delete().is_none());

        // request row 1 (the corrupt row — deleting it is the cleanup path)
        assert!(screen.request_delete(1));
        assert!(screen.is_delete_pending());
        assert_eq!(screen.pending_row(), Some(1));
        // the pending identity matches the requested row's file
        let pending = screen.confirm_delete().unwrap();
        assert_eq!(pending.name, "broken");
        assert_eq!(pending.file_name, "broken.mcvx");
        // consumed: single-use, cleared identity
        assert!(!screen.is_delete_pending());
        assert_eq!(screen.pending_row(), None);
        assert!(screen.confirm_delete().is_none());
    }

    #[test]
    fn delete_cancel_clears_both_the_confirmation_and_the_row_identity() {
        let mut screen = LoadScreen::new();
        screen.set_rows(two_rows());
        screen.request_delete(0);
        assert_eq!(screen.pending_row(), Some(0));
        screen.cancel_delete();
        assert!(!screen.is_delete_pending());
        assert_eq!(screen.pending_row(), None);
        assert!(screen.confirm_delete().is_none());
    }

    #[test]
    fn request_delete_out_of_range_is_ignored_and_replaces_a_prior_request() {
        let mut screen = LoadScreen::new();
        screen.set_rows(two_rows());
        assert!(!screen.request_delete(2), "past the end is ignored");
        assert!(!screen.request_delete(999));
        assert!(!screen.is_delete_pending());

        // a new request replaces the old one: confirm never fires the first row
        assert!(screen.request_delete(0));
        assert!(screen.request_delete(1));
        assert_eq!(screen.pending_row(), Some(1));
        let pending = screen.confirm_delete().unwrap();
        assert_eq!(pending.file_name, "broken.mcvx");
    }

    #[test]
    fn set_rows_replaces_the_catalog_and_cancels_any_pending_delete() {
        let mut screen = LoadScreen::new();
        screen.set_rows(two_rows());
        assert_eq!(screen.row_count(), 2);
        screen.request_delete(0);

        screen.set_rows(vec![CatalogRow::loadable(
            "only".into(),
            "only.mcvx".into(),
            Some(1),
        )]);
        assert_eq!(screen.row_count(), 1);
        assert_eq!(screen.rows()[0].name, "only");
        assert!(
            !screen.is_delete_pending(),
            "a refresh cancels stale pending state"
        );
        assert_eq!(screen.pending_row(), None);
        assert!(screen.confirm_delete().is_none());
    }

    // --- date formatting ---

    #[test]
    fn format_modified_secs_renders_known_dates() {
        // epoch 0 is exactly 1970-01-01
        assert_eq!(format_modified_secs(Some(0)), "1970-01-01");
        // the last second of the first day stays on it
        assert_eq!(format_modified_secs(Some(86_399)), "1970-01-01");
        assert_eq!(format_modified_secs(Some(86_400)), "1970-01-02");
        // leap days: 2000-02-29 and 2024-02-29
        assert_eq!(format_modified_secs(Some(951_782_400)), "2000-02-29");
        assert_eq!(format_modified_secs(Some(1_709_164_800)), "2024-02-29");
        // the day after a leap day
        assert_eq!(format_modified_secs(Some(1_583_020_800)), "2020-03-01");
        // missing timestamp
        assert_eq!(format_modified_secs(None), "unknown");
    }

    #[test]
    fn format_modified_secs_is_total_for_huge_values() {
        // u64::MAX must render without panicking (far-future year, wider
        // than 4 digits is fine — the format is a minimum width).
        let rendered = format_modified_secs(Some(u64::MAX));
        assert!(rendered.len() >= 10, "a date-like string: {rendered}");
    }

    // --- screen-text composition ---

    #[test]
    fn format_row_line_marks_corrupt_rows_and_unknown_dates() {
        let loadable = CatalogRow::loadable("alpha".into(), "alpha.mcvx".into(), Some(0));
        assert_eq!(format_row_line(&loadable), "alpha — 1970-01-01");

        let undated = CatalogRow::loadable("beta".into(), "beta.mcvx".into(), None);
        assert_eq!(format_row_line(&undated), "beta — unknown");

        for problem in [
            RowProblem::InvalidName,
            RowProblem::Unreadable,
            RowProblem::InvalidFormat,
        ] {
            let corrupt =
                CatalogRow::corrupt("bad".into(), "bad.mcvx".into(), Some(86_400), problem);
            assert_eq!(
                format_row_line(&corrupt),
                "bad — 1970-01-02 — (cannot load)",
                "corrupt rows are marked non-loadable for every problem kind"
            );
        }
    }

    #[test]
    fn new_game_row_lines_compose_fields_and_actions_in_order() {
        let mut screen = NewGameScreen::new();
        // empty form, default focus Name: marker on the name, caret at the
        // blank value's start, unfocused blank seed renders the random hint
        assert_eq!(
            new_game_row_lines(&screen),
            vec![
                "> World Name: |".to_string(),
                "  Seed: (random)".to_string(),
                "Randomize Seed".to_string(),
                "Season Length: 1 day".to_string(),
                "Create World".to_string(),
                "Back".to_string(),
            ]
        );

        // filled form: the focused name shows the caret after the text;
        // a whitespace-only unfocused seed still renders the random hint
        screen.apply(TextInputAction::InsertText("Deep Dark".into()));
        screen.cycle_focus();
        screen.apply(TextInputAction::InsertText("  ".into()));
        assert_eq!(
            new_game_row_lines(&screen),
            vec![
                "  World Name: Deep Dark".to_string(),
                "> Seed:   |".to_string(),
                "Randomize Seed".to_string(),
                "Season Length: 1 day".to_string(),
                "Create World".to_string(),
                "Back".to_string(),
            ]
        );

        screen.apply(TextInputAction::InsertText("42".into()));
        assert_eq!(new_game_row_lines(&screen)[1], "> Seed:   42|");
    }

    #[test]
    fn new_game_rows_include_the_season_length_cycler_between_randomize_and_create() {
        assert_eq!(
            NEW_GAME_ROWS,
            [
                NewGameTarget::NameField,
                NewGameTarget::SeedField,
                NewGameTarget::Randomize,
                NewGameTarget::SeasonLength,
                NewGameTarget::Create,
                NewGameTarget::Back,
            ]
        );
        assert_eq!(NEW_GAME_ROW_COUNT, 6);
        assert_eq!(NewGameTarget::SeasonLength.label(), "Season Length");
        // Create/Back shifted down by one: the enum order is the click map.
        assert_eq!(NEW_GAME_ROWS[4], NewGameTarget::Create);
        assert_eq!(NEW_GAME_ROWS[5], NewGameTarget::Back);
    }

    #[test]
    fn season_length_defaults_to_one_day_and_cycles_1_2_4_8_then_wraps() {
        let mut screen = NewGameScreen::new();
        assert_eq!(screen.season_length_days(), 1, "roadmap-pinned default");
        assert_eq!(screen.season_length_index(), 0);
        assert_eq!(
            NEW_GAME_SEASON_LENGTHS[0],
            crate::world::season::DEFAULT_SEASON_LENGTH_DAYS,
            "the row default must match the persistence default"
        );
        assert_eq!(
            new_game_row_lines(&screen)[3],
            "Season Length: 1 day",
            "singular at one day"
        );

        assert_eq!(screen.cycle_season_length(), 2);
        assert_eq!(new_game_row_lines(&screen)[3], "Season Length: 2 days");
        assert_eq!(screen.cycle_season_length(), 4);
        assert_eq!(new_game_row_lines(&screen)[3], "Season Length: 4 days");
        assert_eq!(screen.cycle_season_length(), 8);
        assert_eq!(new_game_row_lines(&screen)[3], "Season Length: 8 days");
        assert_eq!(
            screen.cycle_season_length(),
            1,
            "the last choice wraps back to the default"
        );
        assert_eq!(screen.season_length_index(), 0);
        assert_eq!(new_game_row_lines(&screen)[3], "Season Length: 1 day");
    }

    #[test]
    fn season_length_row_never_joins_the_text_focus_cycle() {
        // Tab still toggles exactly the two text fields; the season-length
        // row is a click-only cycler, so no ScreenKeyAction arm changes.
        let mut screen = NewGameScreen::new();
        assert_eq!(screen.cycle_focus(), FocusField::Seed);
        assert_eq!(screen.cycle_focus(), FocusField::Name);
        assert_eq!(screen.season_length_days(), 1, "Tab never cycles it");
        assert_eq!(
            screen_key_action(LifecycleScreen::NewGame, KeyCode::Tab, false),
            ScreenKeyAction::CycleField
        );
        assert_eq!(
            screen_key_action(LifecycleScreen::NewGame, KeyCode::Enter, false),
            ScreenKeyAction::Create
        );
        // Cycling the value leaves the focused field and both text values
        // untouched (one writer per event).
        screen.apply(TextInputAction::InsertText("Deep Dark".into()));
        screen.cycle_season_length();
        assert_eq!(screen.focus(), FocusField::Name);
        assert_eq!(screen.name().value(), "Deep Dark");
        assert_eq!(screen.season_length_days(), 2);
    }

    #[test]
    fn focus_marker_and_caret_follow_focus_and_edits() {
        let mut screen = NewGameScreen::new();
        // focused marker on the name; caret at index 0 of the blank value
        assert_eq!(new_game_row_lines(&screen)[0], "> World Name: |");
        assert_eq!(new_game_row_lines(&screen)[1], "  Seed: (random)");

        // a single-char insert advances the caret
        screen.apply(TextInputAction::Insert('D'));
        assert_eq!(new_game_row_lines(&screen)[0], "> World Name: D|");
        // InsertText appends and the caret lands after the text
        screen.apply(TextInputAction::InsertText("eep Dark".into()));
        assert_eq!(new_game_row_lines(&screen)[0], "> World Name: Deep Dark|");
        // Backspace removes the char before the caret and moves it back
        screen.apply(TextInputAction::Backspace);
        assert_eq!(new_game_row_lines(&screen)[0], "> World Name: Deep Dar|");
        // the unfocused seed keeps the random hint throughout
        assert_eq!(new_game_row_lines(&screen)[1], "  Seed: (random)");

        // Tab moves the marker to the seed; its blank value shows the
        // caret (not the random hint — the hint is unfocused-only)
        screen.cycle_focus();
        assert_eq!(new_game_row_lines(&screen)[0], "  World Name: Deep Dar");
        assert_eq!(new_game_row_lines(&screen)[1], "> Seed: |");
        // typing in the seed moves its caret and leaves the name untouched
        screen.apply(TextInputAction::InsertText("42".into()));
        assert_eq!(new_game_row_lines(&screen)[1], "> Seed: 42|");
        assert_eq!(new_game_row_lines(&screen)[0], "  World Name: Deep Dar");

        // set_focus (click-to-focus) moves the marker without Tab
        screen.set_focus(FocusField::Name);
        assert_eq!(new_game_row_lines(&screen)[0], "> World Name: Deep Dar|");
        assert_eq!(new_game_row_lines(&screen)[1], "  Seed: 42");
    }

    #[test]
    fn delete_prompt_line_names_the_pending_row() {
        assert_eq!(delete_prompt_line("Overworld"), "Delete 'Overworld'?");
    }

    // --- keyboard routing ---

    #[test]
    fn screen_key_actions_route_by_screen_and_pending_delete_state() {
        // new game
        assert_eq!(
            screen_key_action(LifecycleScreen::NewGame, KeyCode::Escape, false),
            ScreenKeyAction::Back
        );
        assert_eq!(
            screen_key_action(LifecycleScreen::NewGame, KeyCode::Enter, false),
            ScreenKeyAction::Create
        );
        assert_eq!(
            screen_key_action(LifecycleScreen::NewGame, KeyCode::NumpadEnter, false),
            ScreenKeyAction::Create
        );
        assert_eq!(
            screen_key_action(LifecycleScreen::NewGame, KeyCode::Tab, false),
            ScreenKeyAction::CycleField
        );
        assert_eq!(
            screen_key_action(LifecycleScreen::NewGame, KeyCode::ArrowLeft, false),
            ScreenKeyAction::None
        );

        // load game: a pending delete owns Escape and Enter first
        assert_eq!(
            screen_key_action(LifecycleScreen::LoadGame, KeyCode::Escape, false),
            ScreenKeyAction::Back
        );
        assert_eq!(
            screen_key_action(LifecycleScreen::LoadGame, KeyCode::Escape, true),
            ScreenKeyAction::CancelPendingDelete
        );
        assert_eq!(
            screen_key_action(LifecycleScreen::LoadGame, KeyCode::Enter, true),
            ScreenKeyAction::ConfirmPendingDelete
        );
        assert_eq!(
            screen_key_action(LifecycleScreen::LoadGame, KeyCode::Enter, false),
            ScreenKeyAction::None,
            "Enter without a pending delete has no screen-level meaning"
        );
        assert_eq!(
            screen_key_action(LifecycleScreen::LoadGame, KeyCode::Tab, true),
            ScreenKeyAction::None
        );

        // pause: Escape backs out (the caller applies it as Resume)
        assert_eq!(
            screen_key_action(LifecycleScreen::Pause, KeyCode::Escape, false),
            ScreenKeyAction::Back
        );
        assert_eq!(
            screen_key_action(LifecycleScreen::Pause, KeyCode::Enter, false),
            ScreenKeyAction::None
        );

        // main menu: nothing to back out to — a documented no-op
        assert_eq!(
            screen_key_action(LifecycleScreen::MainMenu, KeyCode::Escape, false),
            ScreenKeyAction::None
        );

        // gameplay and settings own their own keys
        assert_eq!(
            screen_key_action(LifecycleScreen::Playing, KeyCode::Escape, false),
            ScreenKeyAction::None
        );
        assert_eq!(
            screen_key_action(
                LifecycleScreen::Settings(crate::ui::SettingsCaller::MainMenu),
                KeyCode::Escape,
                false
            ),
            ScreenKeyAction::None
        );
    }
}
