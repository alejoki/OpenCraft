//! Pure prefab-capture state machine and bounded name prompt.
//!
//! Same purity contract as the other `src/ui` models: no winit event loop,
//! GPU, `WorldManager`, session, clock, or write. The only outside read is the
//! sorted scan of `root/prefabs/*.toml` used to pick a non-colliding default
//! name in [`suggest_name`] (it reads stems plus header-declared names) — it
//! never looks outside that directory.
//!
//! The state machine is deliberately separate from the in-game key handling
//! (named `capture` so it cannot be confused with the settings menu's key
//! capture). `main.rs` translates real [`KeyCode`]/`event.text` pairs into
//! [`capture_key_action`], renders [`PrefabCapture::prompt_line`], and calls
//! [`PrefabCapture::begin_naming`] between the second corner and the prompt.
//! The world write itself stays in `world::capture`.

use std::collections::BTreeSet;
use std::path::Path;

use glam::IVec3;
use winit::keyboard::KeyCode;

use crate::ui::text_input::{TextInput, TextInputAction};
use crate::world::capture::{region_from_corners, CaptureRegion};
use crate::world::prefab::prefab_identities;

/// Maximum prefab name length accepted by the shared name rule.
pub const CAPTURE_NAME_MAX_CHARS: usize = 64;

/// Default base name used when no prefab of that name exists.
pub const DEFAULT_CAPTURE_NAME: &str = "capture";

/// The prompt text shown while [`PrefabCaptureStage::Naming`] is active.
///
/// Pinned: the HUD renders this exact measured line, so tests assert it
/// byte-for-byte rather than reconstructing it.
pub const CAPTURE_PROMPT_SUFFIX: &str = "  [Enter] save  [Esc] cancel";

/// One capture interaction stage.
///
/// `Corners.first` is the raw first corner. A second corner is remembered
/// separately (in [`PrefabCapture`]) only long enough for the caller to call
/// [`PrefabCapture::begin_naming`]; `Naming` owns the normalized region and
/// the editable name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrefabCaptureStage {
    /// No selection in progress.
    Idle,
    /// One corner selected; waiting for the second.
    Corners { first: IVec3 },
    /// A complete region is selected and the name prompt is active.
    Naming {
        region: CaptureRegion,
        name: TextInput,
    },
}

/// The result of pressing the capture key at a world position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CornerOutcome {
    /// The first corner was stored.
    First,
    /// The second corner completed a normalized region.
    Ready(CaptureRegion),
    /// The press cannot advance the selection (already naming or pending).
    Ignored,
}

/// How one keyboard event was classified while capture owns the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureKey {
    /// The event is not a capture key; gameplay routing may continue.
    PassThrough,
    /// The event was an edit or an ignored key while typing.
    Consumed,
    /// The typed name should be saved.
    Submit,
    /// The selection/prompt should be discarded.
    Cancel,
}

/// Pure two-corner selection plus name-prompt state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrefabCapture {
    stage: PrefabCaptureStage,
    /// Region completed by the second corner, held until `begin_naming`.
    pending: Option<CaptureRegion>,
}

impl Default for PrefabCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl PrefabCapture {
    /// Start idle with no corners selected.
    pub fn new() -> Self {
        Self {
            stage: PrefabCaptureStage::Idle,
            pending: None,
        }
    }

    /// The current stage (read-only view for tests and renderers).
    pub fn stage(&self) -> &PrefabCaptureStage {
        &self.stage
    }

    /// Whether any capture interaction is in progress.
    pub fn is_active(&self) -> bool {
        !matches!(self.stage, PrefabCaptureStage::Idle)
    }

    /// Whether the name prompt currently owns the keyboard.
    pub fn is_naming(&self) -> bool {
        matches!(self.stage, PrefabCaptureStage::Naming { .. })
    }

    /// Whether at least one corner is selected (including while naming).
    pub fn has_corners(&self) -> bool {
        matches!(
            self.stage,
            PrefabCaptureStage::Corners { .. } | PrefabCaptureStage::Naming { .. }
        )
    }

    /// Advance the selection with one targeted world block.
    ///
    /// The first press stores the corner; the second normalizes both corners
    /// (order-independent) and returns the inclusive region. Presses while
    /// naming or after a completed pending selection are ignored.
    pub fn press_corner(&mut self, corner: IVec3) -> CornerOutcome {
        match &self.stage {
            PrefabCaptureStage::Idle => {
                self.stage = PrefabCaptureStage::Corners { first: corner };
                CornerOutcome::First
            }
            PrefabCaptureStage::Corners { first } => {
                if self.pending.is_some() {
                    return CornerOutcome::Ignored;
                }
                let region = region_from_corners(*first, corner);
                self.pending = Some(region);
                CornerOutcome::Ready(region)
            }
            PrefabCaptureStage::Naming { .. } => CornerOutcome::Ignored,
        }
    }

    /// Enter the naming stage for a completed selection.
    ///
    /// `root` is the worldgen directory (`None` when the session has no
    /// worldgen root configured); the prompt is prefilled with
    /// [`suggest_name`]. Returns `false` and changes nothing when there is no
    /// completed two-corner selection to name.
    pub fn begin_naming(&mut self, root: Option<&Path>) -> bool {
        let Some(region) = self.pending.take() else {
            return false;
        };
        let name = suggest_name(root);
        self.stage = PrefabCaptureStage::Naming {
            region,
            name: TextInput::from_text(name, CAPTURE_NAME_MAX_CHARS),
        };
        true
    }

    /// The normalized selection box as `(min, max)`, inclusive.
    ///
    /// A single corner draws a one-block box; a completed region (including
    /// while naming) draws the normalized inclusive extent. `None` only while
    /// idle.
    pub fn selection_box(&self) -> Option<(IVec3, IVec3)> {
        match &self.stage {
            PrefabCaptureStage::Idle => None,
            PrefabCaptureStage::Corners { first } => Some(match self.pending {
                Some(region) => (region.min, region.max),
                None => (*first, *first),
            }),
            PrefabCaptureStage::Naming { region, .. } => Some((region.min, region.max)),
        }
    }

    /// The exact one-line prompt while naming, otherwise `None`.
    pub fn prompt_line(&self) -> Option<String> {
        match &self.stage {
            PrefabCaptureStage::Naming { name, .. } => Some(format!(
                "Name prefab: {}{CAPTURE_PROMPT_SUFFIX}",
                name.value()
            )),
            _ => None,
        }
    }

    /// The current name buffer, or `""` when not naming.
    pub fn naming_value(&self) -> &str {
        match &self.stage {
            PrefabCaptureStage::Naming { name, .. } => name.value(),
            _ => "",
        }
    }

    /// Clear every selection and prompt back to [`PrefabCaptureStage::Idle`].
    pub fn reset(&mut self) {
        self.stage = PrefabCaptureStage::Idle;
        self.pending = None;
    }

    /// Classify one key press while capture owns the keyboard.
    ///
    /// Mirrors [`capture_key_action`] without exposing the stage publicly.
    /// Callers should invoke this for press events; releases are consumed by
    /// the caller's naming gate.
    pub fn key_action(&mut self, key: KeyCode, text: Option<&str>) -> CaptureKey {
        capture_key_action(&mut self.stage, key, text)
    }
}

/// Translate one winit key/text pair into a capture action.
///
/// Only [`PrefabCaptureStage::Naming`] consumes input. Enter/NumpadEnter
/// submit and Escape cancels; Backspace/Delete/Left/Right/Home/End apply the
/// matching text edit; printable text is filtered to `a-z0-9_-` (uppercase
/// lowercased) and bounded by [`CAPTURE_NAME_MAX_CHARS`]. Every remaining key
/// is [`CaptureKey::Consumed`] so no gameplay binding can fire while typing.
/// Idle and corner selection keep normal gameplay routing.
pub fn capture_key_action(
    stage: &mut PrefabCaptureStage,
    key: KeyCode,
    text: Option<&str>,
) -> CaptureKey {
    let PrefabCaptureStage::Naming { name, .. } = stage else {
        return CaptureKey::PassThrough;
    };

    match key {
        KeyCode::Enter | KeyCode::NumpadEnter => CaptureKey::Submit,
        KeyCode::Escape => CaptureKey::Cancel,
        KeyCode::Backspace => {
            name.apply(TextInputAction::Backspace);
            CaptureKey::Consumed
        }
        KeyCode::Delete => {
            name.apply(TextInputAction::Delete);
            CaptureKey::Consumed
        }
        KeyCode::ArrowLeft => {
            name.apply(TextInputAction::MoveLeft);
            CaptureKey::Consumed
        }
        KeyCode::ArrowRight => {
            name.apply(TextInputAction::MoveRight);
            CaptureKey::Consumed
        }
        KeyCode::Home => {
            name.apply(TextInputAction::Home);
            CaptureKey::Consumed
        }
        KeyCode::End => {
            name.apply(TextInputAction::End);
            CaptureKey::Consumed
        }
        _ => {
            if let Some(text) = text {
                for character in text.chars() {
                    if let Some(character) = capture_character(character) {
                        name.apply(TextInputAction::Insert(character));
                    }
                }
            }
            CaptureKey::Consumed
        }
    }
}

/// Compose the one measured gameplay notice line.
///
/// The editable prompt keeps precedence over the transient message, but when
/// both are present the message is appended after ` | ` instead of being
/// suppressed. That is what keeps a failed capture's [`CaptureError`] reason
/// readable while the prompt stays on screen for a retry. `None` means this
/// frame draws no notice at all.
///
/// [`CaptureError`]: crate::world::capture::CaptureError
pub fn capture_notice_line(prompt: Option<&str>, message: Option<&str>) -> Option<String> {
    match (prompt, message) {
        (Some(prompt), Some(message)) => Some(format!("{prompt} | {message}")),
        (Some(prompt), None) => Some(prompt.to_owned()),
        (None, Some(message)) => Some(message.to_owned()),
        (None, None) => None,
    }
}

/// Pick the first free deterministic prefab name.
///
/// Reads only `root/prefabs`; a missing or unreadable tree (and a `None`
/// root) falls back to [`DEFAULT_CAPTURE_NAME`]. Every claimed stem and every
/// header-declared name is collected, then `capture`, `capture_2`,
/// `capture_3`, ... is returned — so a renamed file that declares `capture`
/// can never be suggested as a fresh target.
pub fn suggest_name(root: Option<&Path>) -> String {
    let Some(root) = root else {
        return DEFAULT_CAPTURE_NAME.to_owned();
    };
    let taken = existing_prefab_names(root);
    let mut counter: u64 = 1;
    loop {
        let candidate = if counter == 1 {
            DEFAULT_CAPTURE_NAME.to_owned()
        } else {
            format!("{DEFAULT_CAPTURE_NAME}_{counter}")
        };
        if !taken.contains(&candidate) {
            return candidate;
        }
        counter = counter.saturating_add(1);
    }
}

/// Collect every stem and declared name claimed in `root/prefabs`.
fn existing_prefab_names(root: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for identity in prefab_identities(root) {
        names.insert(identity.stem);
        if let Some(declared) = identity.declared_name {
            names.insert(declared);
        }
    }
    names
}

/// One accepted name character: lowercase ASCII, digits, `_` and `-`.
fn capture_character(character: char) -> Option<char> {
    let lowered = character.to_ascii_lowercase();
    if lowered.is_ascii_lowercase() || lowered.is_ascii_digit() || matches!(lowered, '_' | '-') {
        Some(lowered)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::world::prefab::PREFABS_SUBDIR;

    use super::*;

    fn temp_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn naming_capture(name: &str) -> PrefabCapture {
        let mut capture = PrefabCapture::new();
        assert_eq!(
            capture.press_corner(IVec3::new(0, 40, 0)),
            CornerOutcome::First
        );
        let region = match capture.press_corner(IVec3::new(2, 42, 3)) {
            CornerOutcome::Ready(region) => region,
            other => panic!("expected ready, got {other:?}"),
        };
        capture.stage = PrefabCaptureStage::Naming {
            region,
            name: TextInput::from_text(name, CAPTURE_NAME_MAX_CHARS),
        };
        capture.pending = None;
        capture
    }

    #[test]
    fn first_corner_starts_selection_and_draws_one_block_box() {
        let mut capture = PrefabCapture::new();
        assert!(!capture.is_active());
        assert!(!capture.has_corners());
        assert_eq!(capture.selection_box(), None);
        assert_eq!(capture.prompt_line(), None);
        assert_eq!(capture.naming_value(), "");

        let corner = IVec3::new(-4, 41, 7);
        assert_eq!(capture.press_corner(corner), CornerOutcome::First);
        assert!(capture.is_active());
        assert!(capture.has_corners());
        assert!(!capture.is_naming());
        assert_eq!(capture.selection_box(), Some((corner, corner)));
    }

    #[test]
    fn second_corner_normalizes_reversed_corners() {
        let mut capture = PrefabCapture::new();
        let a = IVec3::new(5, 44, -2);
        let b = IVec3::new(1, 40, 3);
        assert_eq!(capture.press_corner(a), CornerOutcome::First);
        let region = match capture.press_corner(b) {
            CornerOutcome::Ready(region) => region,
            other => panic!("expected ready, got {other:?}"),
        };
        assert_eq!(region, region_from_corners(a, b));
        assert_eq!(region.min, IVec3::new(1, 40, -2));
        assert_eq!(region.max, IVec3::new(5, 44, 3));
        assert_eq!(capture.selection_box(), Some((region.min, region.max)));
        assert_eq!(
            capture.press_corner(IVec3::new(9, 9, 9)),
            CornerOutcome::Ignored
        );
    }

    #[test]
    fn begin_naming_prefills_capture_and_pins_the_prompt() {
        let mut capture = PrefabCapture::new();
        capture.press_corner(IVec3::new(0, 40, 0));
        capture.press_corner(IVec3::new(1, 41, 1));

        assert!(capture.begin_naming(None));
        assert!(capture.is_naming());
        assert!(capture.has_corners());
        assert_eq!(capture.naming_value(), "capture");
        assert_eq!(
            capture.prompt_line().as_deref(),
            Some("Name prefab: capture  [Enter] save  [Esc] cancel")
        );
        assert_eq!(
            capture.selection_box(),
            Some((IVec3::new(0, 40, 0), IVec3::new(1, 41, 1)))
        );
        assert!(!capture.begin_naming(None), "naming cannot start twice");
    }

    #[test]
    fn begin_naming_requires_a_completed_selection() {
        let mut capture = PrefabCapture::new();
        assert!(!capture.begin_naming(None));
        assert!(!capture.is_active());

        capture.press_corner(IVec3::new(2, 40, 2));
        assert!(!capture.begin_naming(None));
        assert_eq!(
            capture.stage(),
            &PrefabCaptureStage::Corners {
                first: IVec3::new(2, 40, 2)
            }
        );
    }

    #[test]
    fn press_corner_while_naming_is_ignored() {
        let mut capture = naming_capture("house");
        assert_eq!(
            capture.press_corner(IVec3::new(9, 50, 9)),
            CornerOutcome::Ignored
        );
        assert_eq!(capture.naming_value(), "house");
    }

    #[test]
    fn prompt_edits_round_trip_through_cursor_actions() {
        let mut capture = naming_capture("");
        assert_eq!(
            capture.key_action(KeyCode::KeyH, Some("h")),
            CaptureKey::Consumed
        );
        assert_eq!(
            capture.key_action(KeyCode::KeyO, Some("o")),
            CaptureKey::Consumed
        );
        assert_eq!(
            capture.key_action(KeyCode::KeyU, Some("u")),
            CaptureKey::Consumed
        );
        assert_eq!(
            capture.key_action(KeyCode::KeyS, Some("s")),
            CaptureKey::Consumed
        );
        assert_eq!(
            capture.key_action(KeyCode::KeyE, Some("e")),
            CaptureKey::Consumed
        );
        assert_eq!(capture.naming_value(), "house");
        assert_eq!(
            capture.prompt_line().as_deref(),
            Some("Name prefab: house  [Enter] save  [Esc] cancel")
        );

        assert_eq!(
            capture.key_action(KeyCode::Backspace, None),
            CaptureKey::Consumed
        );
        assert_eq!(capture.naming_value(), "hous");
        assert_eq!(
            capture.key_action(KeyCode::Home, None),
            CaptureKey::Consumed
        );
        assert_eq!(
            capture.key_action(KeyCode::KeyX, Some("x")),
            CaptureKey::Consumed
        );
        assert_eq!(capture.naming_value(), "xhous");
        assert_eq!(capture.key_action(KeyCode::End, None), CaptureKey::Consumed);
        assert_eq!(
            capture.key_action(KeyCode::ArrowLeft, None),
            CaptureKey::Consumed
        );
        assert_eq!(
            capture.key_action(KeyCode::Delete, None),
            CaptureKey::Consumed
        );
        assert_eq!(capture.naming_value(), "xhou");
        assert_eq!(
            capture.key_action(KeyCode::KeyA, Some("a")),
            CaptureKey::Consumed
        );
        assert_eq!(capture.naming_value(), "xhoua");
    }

    #[test]
    fn enter_submits_and_escape_cancels() {
        let mut capture = naming_capture("keep");
        assert_eq!(capture.key_action(KeyCode::Enter, None), CaptureKey::Submit);
        assert_eq!(capture.naming_value(), "keep");
        assert_eq!(
            capture.key_action(KeyCode::NumpadEnter, None),
            CaptureKey::Submit
        );
        assert_eq!(
            capture.key_action(KeyCode::Escape, None),
            CaptureKey::Cancel
        );
    }

    #[test]
    fn every_other_key_is_consumed_while_naming() {
        let mut capture = naming_capture("abc");
        for key in [
            KeyCode::KeyW,
            KeyCode::Space,
            KeyCode::Tab,
            KeyCode::F5,
            KeyCode::Digit1,
            KeyCode::ShiftLeft,
            KeyCode::ArrowUp,
            KeyCode::KeyQ,
        ] {
            assert_eq!(
                capture.key_action(key, None),
                CaptureKey::Consumed,
                "{key:?} must not leak to gameplay"
            );
        }
        assert_eq!(capture.naming_value(), "abc");
    }

    #[test]
    fn idle_and_corner_stages_pass_through() {
        let mut capture = PrefabCapture::new();
        assert_eq!(
            capture.key_action(KeyCode::KeyP, None),
            CaptureKey::PassThrough
        );
        capture.press_corner(IVec3::new(0, 40, 0));
        assert_eq!(
            capture.key_action(KeyCode::Enter, None),
            CaptureKey::PassThrough
        );
        assert_eq!(
            capture.key_action(KeyCode::Escape, None),
            CaptureKey::PassThrough
        );
    }

    #[test]
    fn disallowed_characters_are_filtered_and_uppercase_lowercased() {
        let mut capture = naming_capture("");
        assert_eq!(
            capture.key_action(KeyCode::KeyA, Some("A.B/C\\D E")),
            CaptureKey::Consumed
        );
        assert_eq!(capture.naming_value(), "abcde");
        assert_eq!(
            capture.key_action(KeyCode::KeyF, Some("Hé_9-B")),
            CaptureKey::Consumed
        );
        assert_eq!(capture.naming_value(), "abcdeh_9-b");
    }

    #[test]
    fn name_is_bounded_to_64_characters() {
        let mut capture = naming_capture("");
        let text: String = "a".repeat(100);
        assert_eq!(
            capture.key_action(KeyCode::KeyA, Some(&text)),
            CaptureKey::Consumed
        );
        assert_eq!(capture.naming_value().len(), CAPTURE_NAME_MAX_CHARS);
        assert_eq!(capture.naming_value(), "a".repeat(64));
    }

    #[test]
    fn reset_returns_to_idle_and_clears_the_box() {
        let mut capture = naming_capture("gone");
        capture.reset();
        assert_eq!(capture.stage(), &PrefabCaptureStage::Idle);
        assert!(!capture.is_active());
        assert!(!capture.is_naming());
        assert!(!capture.has_corners());
        assert_eq!(capture.selection_box(), None);
        assert_eq!(capture.prompt_line(), None);
        assert_eq!(capture.naming_value(), "");
        assert!(!capture.begin_naming(None));
    }

    #[test]
    fn suggest_name_avoids_existing_files_in_sorted_order() {
        let temp = temp_root();
        let prefabs = temp.path().join(PREFABS_SUBDIR);
        fs::create_dir_all(&prefabs).expect("prefabs dir");
        for name in ["capture.toml", "capture_2.toml", "unrelated.txt"] {
            fs::write(prefabs.join(name), b"placeholder").expect("write placeholder");
        }
        assert_eq!(suggest_name(Some(temp.path())), "capture_3");

        fs::write(prefabs.join("capture_3.toml"), b"placeholder").expect("write placeholder");
        assert_eq!(suggest_name(Some(temp.path())), "capture_4");

        fs::create_dir_all(prefabs.join("capture_4.toml")).expect("dir named like a prefab");
        assert_eq!(suggest_name(Some(temp.path())), "capture_5");
    }

    #[test]
    fn suggest_name_missing_or_unreadable_tree_is_capture() {
        let temp = temp_root();
        assert_eq!(suggest_name(Some(temp.path())), "capture");
        assert_eq!(suggest_name(None), "capture");

        let mut capture = PrefabCapture::new();
        capture.press_corner(IVec3::new(0, 40, 0));
        capture.press_corner(IVec3::new(0, 40, 0));
        assert!(capture.begin_naming(Some(temp.path())));
        assert_eq!(capture.naming_value(), "capture");
    }

    #[test]
    fn prefill_uses_a_free_name_from_the_root() {
        let temp = temp_root();
        let prefabs = temp.path().join(PREFABS_SUBDIR);
        fs::create_dir_all(&prefabs).expect("prefabs dir");
        fs::write(prefabs.join("capture.toml"), b"placeholder").expect("write placeholder");

        let mut capture = PrefabCapture::new();
        capture.press_corner(IVec3::new(1, 41, 1));
        capture.press_corner(IVec3::new(1, 41, 1));
        assert!(capture.begin_naming(Some(temp.path())));
        assert_eq!(capture.naming_value(), "capture_2");
    }

    #[test]
    fn full_corner_naming_submit_flow_yields_the_typed_name() {
        let mut capture = PrefabCapture::new();
        assert_eq!(
            capture.press_corner(IVec3::new(-1, 40, 2)),
            CornerOutcome::First
        );
        assert_eq!(
            capture.press_corner(IVec3::new(3, 43, -2)),
            CornerOutcome::Ready(region_from_corners(
                IVec3::new(-1, 40, 2),
                IVec3::new(3, 43, -2)
            ))
        );
        assert!(capture.begin_naming(None));
        assert_eq!(capture.naming_value(), "capture");
        for _ in 0.."capture".len() {
            assert_eq!(
                capture.key_action(KeyCode::Backspace, None),
                CaptureKey::Consumed
            );
        }
        assert_eq!(capture.naming_value(), "");
        for character in "My_House-2".chars() {
            assert_eq!(
                capture.key_action(KeyCode::KeyM, Some(&character.to_string())),
                CaptureKey::Consumed
            );
        }
        assert_eq!(capture.naming_value(), "my_house-2");
        assert_eq!(capture.key_action(KeyCode::Enter, None), CaptureKey::Submit);
        assert_eq!(capture.naming_value(), "my_house-2");
    }

    #[test]
    fn notice_composition_keeps_the_prompt_and_adds_the_reason() {
        use crate::world::capture::CaptureError;

        // Prompt only: the measured line is byte-identical to prompt_line.
        let prompt = "Name prefab: capture  [Enter] save  [Esc] cancel";
        assert_eq!(
            capture_notice_line(Some(prompt), None).as_deref(),
            Some(prompt)
        );

        // Message only: a plain transient notice passes through untouched.
        assert_eq!(
            capture_notice_line(None, Some("Chest closed")).as_deref(),
            Some("Chest closed")
        );

        // Prompt + captured failure: the prompt stays the editable prefix and
        // the CaptureError Display reason is appended (never suppressed).
        let error = CaptureError::UnloadedChunk {
            pos: crate::world::chunk::ChunkPos::new(-1, 2, 3),
        };
        let composed = capture_notice_line(Some(prompt), Some(&error.to_string()))
            .expect("prompt and reason compose");
        assert!(composed.starts_with(prompt), "{composed}");
        assert!(
            composed.contains(&error.to_string()),
            "the CaptureError Display reason must be readable: {composed}"
        );
        assert_eq!(composed, format!("{prompt} | {error}"));

        // Success reset: the prompt is gone, so the saved line stands alone.
        let saved = "saved prefab 'capture' to worldgen/prefabs/capture.toml";
        assert_eq!(
            capture_notice_line(None, Some(saved)).as_deref(),
            Some(saved)
        );

        // Neither line: no notice is drawn.
        assert_eq!(capture_notice_line(None, None), None);
    }

    #[test]
    fn suggest_name_never_proposes_a_declared_name_collision() {
        let temp = temp_root();
        let prefabs = temp.path().join(PREFABS_SUBDIR);
        fs::create_dir_all(&prefabs).expect("prefabs dir");
        // A renamed file whose header claims the default name: its stem is
        // free, but `capture` is already taken by declaration.
        fs::write(
            prefabs.join("renamed.toml"),
            br###"format = 1
name = "capture"
origin = [0, 0, 0]
size = [2, 1, 1]

layers = [
  [ "##" ],
]

[palette]
"#" = "stone"
"###,
        )
        .expect("write renamed prefab");

        assert_eq!(suggest_name(Some(temp.path())), "capture_2");
    }
}
