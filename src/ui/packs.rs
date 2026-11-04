//! Pure Packs screen logic: the ordered-stack row model, pixel-space
//! geometry, hit-testing, stack transitions, and text composition.
//!
//! Same purity contract as [`super::screens`] and [`super::menu`]: no GPU,
//! no event loop, no clock, no filesystem. Every input is an argument and
//! every output is derived state, so the row order, the click targets, and
//! the toggle/reorder rules are unit-testable on CPU alone. `main.rs`
//! (T07) owns the frame loop and applies these helpers each frame, so a
//! `Resized` reflows the screen automatically. Coordinates are physical
//! pixels, top-left origin, y down — the same space as `Quad`,
//! `ctx.config`, glyphon `TextArea`, and `GameApp.cursor_pos`
//! (P003/MEM012). Hit tests reuse [`menu::hit_test`] (half-open like the
//! pixel grid).

use super::menu::{hit_test, Rect, MENU_PAD, MENU_TOP};
use super::screens::SCREEN_WIDTH;
use super::LINE_HEIGHT;

/// Width in px of each per-row reorder action target (Up/Down).
pub const PACK_ACTION_WIDTH: f32 = 44.0;
/// Panel width in px for the Packs screen, matching the other simple
/// screens so they share one visual width.
pub const PACK_SCREEN_WIDTH: f32 = SCREEN_WIDTH;

/// One row of the Packs screen, in display order.
///
/// Enabled rows come first in stack order; disabled rows follow in
/// alphabetical order. `missing` marks a stack entry whose folder no
/// longer exists under `packs/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackRow {
    pub name: String,
    pub enabled: bool,
    pub missing: bool,
}

/// Build the display rows for the given available pack folders and the
/// authored stack.
///
/// Enabled rows are the stack in order (a name with no folder is marked
/// `missing`); disabled rows are the available names not in the stack,
/// sorted alphabetically. No name appears twice, and an empty stack lists
/// every available pack disabled.
pub fn packs_rows(available: &[String], stack: &[String]) -> Vec<PackRow> {
    let mut rows: Vec<PackRow> = Vec::new();
    for name in stack {
        if rows.iter().any(|row| row.name == *name) {
            continue;
        }
        rows.push(PackRow {
            name: name.clone(),
            enabled: true,
            missing: !available
                .iter()
                .any(|available_name| available_name == name),
        });
    }

    let mut disabled: Vec<&String> = available
        .iter()
        .filter(|name| !stack.iter().any(|stacked| stacked == *name))
        .collect();
    disabled.sort();
    for name in disabled {
        if rows.iter().any(|row| row.name == *name) {
            continue;
        }
        rows.push(PackRow {
            name: name.clone(),
            enabled: false,
            missing: false,
        });
    }
    rows
}

/// Direction of a one-step stack move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveDirection {
    Up,
    Down,
}

/// Toggle `name` in the stack: append it when absent, remove it when
/// present. Never creates a duplicate.
pub fn toggle_pack(stack: &mut Vec<String>, name: &str) {
    if let Some(index) = stack.iter().position(|stacked| stacked == name) {
        stack.remove(index);
    } else {
        stack.push(name.to_string());
    }
}

/// Move `name` one step up or down in the stack. Returns `false` and
/// leaves the stack unchanged when the name is absent or already at the
/// requested edge.
pub fn move_pack(stack: &mut [String], name: &str, direction: MoveDirection) -> bool {
    let Some(index) = stack.iter().position(|stacked| stacked == name) else {
        return false;
    };
    match direction {
        MoveDirection::Up => {
            if index == 0 {
                return false;
            }
            stack.swap(index, index - 1);
        }
        MoveDirection::Down => {
            if index + 1 >= stack.len() {
                return false;
            }
            stack.swap(index, index + 1);
        }
    }
    true
}

/// Unique stack names in authored order (defensive against a duplicated
/// stack loaded from a hand-edited settings file).
fn unique_names(stack: &[String]) -> Vec<&str> {
    let mut unique: Vec<&str> = Vec::new();
    for name in stack {
        if !unique.contains(&name.as_str()) {
            unique.push(name.as_str());
        }
    }
    unique
}

/// Number of distinct enabled packs in the stack.
pub fn enabled_count(stack: &[String]) -> usize {
    unique_names(stack).len()
}

/// Label for the stack summary: `"Default"` when empty, the pack name
/// when exactly one, and `"<n> enabled"` when more.
pub fn summary_label(stack: &[String]) -> String {
    let unique = unique_names(stack);
    match unique.as_slice() {
        [] => "Default".to_string(),
        [name] => (*name).to_string(),
        names => format!("{} enabled", names.len()),
    }
}

/// Left edge of a centered, fixed-width panel.
fn centered_x(surface_w: u32, width: f32) -> f32 {
    ((surface_w as f32 - width) / 2.0).max(0.0)
}

/// Top edge of row `index`: top-anchored under the stats overlay, same as
/// `screens::row_y`.
fn row_y(index: usize) -> f32 {
    MENU_TOP + MENU_PAD + index as f32 * LINE_HEIGHT
}

/// Rect of row `index` at the full panel width.
fn row_rect(surface_w: u32, index: usize) -> Rect {
    (
        centered_x(surface_w, PACK_SCREEN_WIDTH),
        row_y(index),
        PACK_SCREEN_WIDTH,
        LINE_HEIGHT,
    )
}

/// The dark panel rect behind `row_count` pack rows plus the Back row.
pub fn pack_panel_rect(surface_w: u32, row_count: usize) -> Rect {
    (
        centered_x(surface_w, PACK_SCREEN_WIDTH) - MENU_PAD,
        MENU_TOP,
        PACK_SCREEN_WIDTH + 2.0 * MENU_PAD,
        2.0 * MENU_PAD + (row_count + 1) as f32 * LINE_HEIGHT,
    )
}

/// Geometry of pack row `index`, or `None` out of range.
pub fn pack_row_rect(surface_w: u32, index: usize, row_count: usize) -> Option<Rect> {
    (index < row_count).then(|| row_rect(surface_w, index))
}

/// Rect of the second-from-right action slice ("up") of row `index`.
pub fn pack_up_rect(surface_w: u32, index: usize) -> Rect {
    let (x, y, w, h) = row_rect(surface_w, index);
    (x + w - 2.0 * PACK_ACTION_WIDTH, y, PACK_ACTION_WIDTH, h)
}

/// Rect of the rightmost action slice ("down") of row `index`.
pub fn pack_down_rect(surface_w: u32, index: usize) -> Rect {
    let (x, y, w, h) = row_rect(surface_w, index);
    (x + w - PACK_ACTION_WIDTH, y, PACK_ACTION_WIDTH, h)
}

/// Geometry of the Back row, which sits directly after the last pack row.
pub fn pack_back_rect(surface_w: u32, row_count: usize) -> Rect {
    row_rect(surface_w, row_count)
}

/// What the cursor is over on the Packs screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackTarget {
    /// The row body (toggles the pack).
    Row(usize),
    /// Move the enabled row up one stack position.
    Up(usize),
    /// Move the enabled row down one stack position.
    Down(usize),
    /// Leave the screen.
    Back,
}

/// Which Packs screen target the cursor is over, if any.
///
/// Up/Down are live only on enabled rows and only when the move is
/// possible (`index > 0` for Up, another enabled row after it for Down).
/// Any other click inside a row is a row-body click. `surface_h` is
/// ignored, exactly like the simple `screens::hovered_*` helpers.
pub fn hovered_pack_target(
    cursor: (f32, f32),
    surface_w: u32,
    surface_h: u32,
    rows: &[PackRow],
) -> Option<PackTarget> {
    let _ = surface_h;
    let enabled = rows.iter().filter(|row| row.enabled).count();
    for (index, row) in rows.iter().enumerate() {
        let Some(rect) = pack_row_rect(surface_w, index, rows.len()) else {
            continue;
        };
        if !hit_test(cursor, rect) {
            continue;
        }
        if row.enabled {
            if index > 0 && hit_test(cursor, pack_up_rect(surface_w, index)) {
                return Some(PackTarget::Up(index));
            }
            if index + 1 < enabled && hit_test(cursor, pack_down_rect(surface_w, index)) {
                return Some(PackTarget::Down(index));
            }
        }
        return Some(PackTarget::Row(index));
    }
    if hit_test(cursor, pack_back_rect(surface_w, rows.len())) {
        return Some(PackTarget::Back);
    }
    None
}

/// Compose the screen text: one line per row (`[x]`/`[ ]` plus an
/// optional `(missing)` marker), then `Back`, then an optional message.
/// One line per row lets the renderer anchor each line to its rect.
pub fn compose_pack_lines(rows: &[PackRow], message: Option<&str>) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(rows.len() + 2);
    if rows.is_empty() {
        lines.push("(no packs found)".to_string());
    } else {
        for row in rows {
            let marker = if row.enabled { "[x]" } else { "[ ]" };
            let missing = if row.missing { " (missing)" } else { "" };
            lines.push(format!("{marker} {}{missing}", row.name));
        }
    }
    lines.push("Back".to_string());
    if let Some(message) = message {
        lines.push(message.to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_string()).collect()
    }

    fn rows(available: &[&str], stack: &[&str]) -> Vec<PackRow> {
        packs_rows(&strs(available), &strs(stack))
    }

    fn center(rect: Rect) -> (f32, f32) {
        let (x, y, w, h) = rect;
        (x + w / 2.0, y + h / 2.0)
    }

    // -- rows -----------------------------------------------------------

    #[test]
    fn rows_put_enabled_in_stack_order_then_disabled_alphabetically() {
        let rows = rows(&["beta", "alpha", "gamma"], &["gamma", "beta"]);
        assert_eq!(
            rows,
            vec![
                PackRow {
                    name: "gamma".into(),
                    enabled: true,
                    missing: false
                },
                PackRow {
                    name: "beta".into(),
                    enabled: true,
                    missing: false
                },
                PackRow {
                    name: "alpha".into(),
                    enabled: false,
                    missing: false
                },
            ]
        );
    }

    #[test]
    fn rows_mark_a_stack_entry_with_no_folder_as_missing() {
        let rows = rows(&["alpha"], &["deleted"]);
        assert_eq!(rows[0].name, "deleted");
        assert!(rows[0].enabled);
        assert!(rows[0].missing);
        assert_eq!(
            rows[1],
            PackRow {
                name: "alpha".into(),
                enabled: false,
                missing: false
            }
        );
    }

    #[test]
    fn rows_follow_stack_reorder() {
        let first = rows(&["alpha", "beta"], &["alpha", "beta"]);
        let second = rows(&["alpha", "beta"], &["beta", "alpha"]);
        assert_eq!(
            first.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
        assert_eq!(
            second.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["beta", "alpha"]
        );
    }

    #[test]
    fn rows_list_every_available_pack_disabled_when_stack_is_empty() {
        let rows = rows(&["beta", "alpha", "gamma"], &[]);
        assert!(rows.iter().all(|row| !row.enabled && !row.missing));
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "beta", "gamma"]
        );
    }

    #[test]
    fn rows_never_duplicate_an_enabled_name_as_disabled() {
        let rows = rows(&["alpha", "beta"], &["alpha"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter().filter(|row| row.name == "alpha").count(),
            1,
            "an enabled name must not be repeated in the disabled section"
        );
        assert_eq!(rows.iter().filter(|row| row.name == "beta").count(), 1);
    }

    #[test]
    fn rows_never_duplicate_a_repeated_stack_or_available_entry() {
        let rows = packs_rows(
            &strs(&["alpha", "alpha", "beta"]),
            &strs(&["alpha", "alpha"]),
        );
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
    }

    // -- transitions ----------------------------------------------------

    #[test]
    fn toggle_appends_an_absent_pack_at_the_end() {
        let mut stack = strs(&["alpha"]);
        toggle_pack(&mut stack, "beta");
        assert_eq!(stack, strs(&["alpha", "beta"]));
    }

    #[test]
    fn toggle_removes_a_present_pack_and_is_idempotent() {
        let mut stack = strs(&["alpha", "beta", "gamma"]);
        toggle_pack(&mut stack, "beta");
        assert_eq!(stack, strs(&["alpha", "gamma"]));
        toggle_pack(&mut stack, "beta");
        assert_eq!(stack, strs(&["alpha", "gamma", "beta"]));
        let after_first_add = stack.clone();
        toggle_pack(&mut stack, "beta");
        assert_eq!(stack, strs(&["alpha", "gamma"]));
        toggle_pack(&mut stack, "beta");
        assert_eq!(stack, after_first_add, "toggling twice restores the state");
    }

    #[test]
    fn toggle_never_introduces_a_duplicate() {
        let mut stack = strs(&["alpha"]);
        toggle_pack(&mut stack, "alpha");
        toggle_pack(&mut stack, "alpha");
        assert_eq!(stack, strs(&["alpha"]));
    }

    #[test]
    fn move_pack_swaps_neighbors() {
        let mut stack = strs(&["alpha", "beta"]);
        assert!(move_pack(&mut stack, "beta", MoveDirection::Up));
        assert_eq!(stack, strs(&["beta", "alpha"]));
        assert!(move_pack(&mut stack, "beta", MoveDirection::Down));
        assert_eq!(stack, strs(&["alpha", "beta"]));
    }

    #[test]
    fn move_pack_at_the_edges_returns_false_and_leaves_stack_unchanged() {
        let mut stack = strs(&["alpha", "beta"]);
        assert!(!move_pack(&mut stack, "alpha", MoveDirection::Up));
        assert!(!move_pack(&mut stack, "beta", MoveDirection::Down));
        assert!(!move_pack(&mut stack, "missing", MoveDirection::Up));
        assert_eq!(stack, strs(&["alpha", "beta"]));
    }

    #[test]
    fn enabled_count_and_summary_label_cover_empty_single_and_multi() {
        assert_eq!(enabled_count(&[]), 0);
        assert_eq!(summary_label(&[]), "Default");
        assert_eq!(enabled_count(&strs(&["alpha"])), 1);
        assert_eq!(summary_label(&strs(&["alpha"])), "alpha");
        assert_eq!(enabled_count(&strs(&["alpha", "beta"])), 2);
        assert_eq!(summary_label(&strs(&["alpha", "beta"])), "2 enabled");
        assert_eq!(
            summary_label(&strs(&["alpha", "alpha"])),
            "alpha",
            "repeated stack entries count once"
        );
    }

    // -- geometry and hit-testing --------------------------------------

    #[test]
    fn row_rects_are_contiguous_and_line_height_tall() {
        let first = pack_row_rect(800, 0, 3).unwrap();
        let second = pack_row_rect(800, 1, 3).unwrap();
        let third = pack_row_rect(800, 2, 3).unwrap();
        assert_eq!(first.3, LINE_HEIGHT);
        assert_eq!(first.1 + first.3, second.1);
        assert_eq!(second.1 + second.3, third.1);
        assert_eq!(first.0, second.0);
        assert_eq!(pack_row_rect(800, 3, 3), None);
    }

    #[test]
    fn panel_height_wraps_rows_back_and_padding() {
        let panel = pack_panel_rect(800, 2);
        assert_eq!(panel.1, MENU_TOP);
        assert_eq!(
            panel.3,
            2.0 * MENU_PAD + 3.0 * LINE_HEIGHT,
            "rows + Back line + padding"
        );
        assert_eq!(panel.0, pack_row_rect(800, 0, 2).unwrap().0 - MENU_PAD);
    }

    #[test]
    fn back_rect_is_the_row_after_the_last_pack_row() {
        let back = pack_back_rect(800, 2);
        let last = pack_row_rect(800, 1, 2).unwrap();
        assert_eq!(back.1, last.1 + last.3);
        assert_eq!(back.0, last.0);
        assert_eq!(back.3, LINE_HEIGHT);
    }

    #[test]
    fn up_and_down_actions_are_right_edge_slices() {
        let row = pack_row_rect(800, 1, 3).unwrap();
        let up = pack_up_rect(800, 1);
        let down = pack_down_rect(800, 1);
        assert_eq!(up.2, PACK_ACTION_WIDTH);
        assert_eq!(down.2, PACK_ACTION_WIDTH);
        assert_eq!(up.0 + up.2, down.0);
        assert_eq!(down.0 + down.2, row.0 + row.2);
        assert_eq!((up.1, down.1), (row.1, row.1));
    }

    #[test]
    fn hovered_row_body_returns_row_and_disabled_rows_never_offer_actions() {
        let rows = rows(&["alpha", "beta", "gamma"], &["gamma", "beta"]);
        let row0 = pack_row_rect(800, 0, rows.len()).unwrap();
        let row2 = pack_row_rect(800, 2, rows.len()).unwrap();
        // left half of an enabled row is the toggle body
        assert_eq!(
            hovered_pack_target((row0.0 + 4.0, row0.1 + 4.0), 800, 600, &rows),
            Some(PackTarget::Row(0))
        );
        // a disabled row is a toggle body across its whole width
        let down2 = pack_down_rect(800, 2);
        assert_eq!(
            hovered_pack_target(center(down2), 800, 600, &rows),
            Some(PackTarget::Row(2))
        );
        assert_eq!(
            hovered_pack_target(center(row2), 800, 600, &rows),
            Some(PackTarget::Row(2))
        );
    }

    #[test]
    fn up_is_hittable_only_for_enabled_rows_with_a_predecessor() {
        let rows = rows(&["alpha", "beta", "gamma"], &["gamma", "beta"]);
        let up0 = pack_up_rect(800, 0);
        let up1 = pack_up_rect(800, 1);
        // row 0 is the first enabled row: no move up, so it toggles
        assert_eq!(
            hovered_pack_target(center(up0), 800, 600, &rows),
            Some(PackTarget::Row(0))
        );
        // row 1 has an enabled predecessor
        assert_eq!(
            hovered_pack_target(center(up1), 800, 600, &rows),
            Some(PackTarget::Up(1))
        );
    }

    #[test]
    fn down_is_hittable_only_when_another_enabled_row_follows() {
        let rows = rows(&["alpha", "beta", "gamma"], &["gamma", "beta"]);
        let down0 = pack_down_rect(800, 0);
        let down1 = pack_down_rect(800, 1);
        assert_eq!(
            hovered_pack_target(center(down0), 800, 600, &rows),
            Some(PackTarget::Down(0))
        );
        // row 1 is the last enabled row: no move down, so it toggles
        assert_eq!(
            hovered_pack_target(center(down1), 800, 600, &rows),
            Some(PackTarget::Row(1))
        );
    }

    #[test]
    fn back_and_outside_targets() {
        let rows = rows(&["alpha"], &["alpha"]);
        let back = pack_back_rect(800, rows.len());
        assert_eq!(
            hovered_pack_target(center(back), 800, 600, &rows),
            Some(PackTarget::Back)
        );
        assert_eq!(hovered_pack_target((0.0, 0.0), 800, 600, &rows), None);
        assert_eq!(
            hovered_pack_target((10_000.0, 10_000.0), 800, 600, &rows),
            None
        );
        assert_eq!(hovered_pack_target((400.0, 600.0), 800, 600, &[]), None);
    }

    #[test]
    fn a_missing_row_still_toggles() {
        let mut stack = strs(&["deleted"]);
        let rows = packs_rows(&[], &stack);
        let row = pack_row_rect(800, 0, rows.len()).unwrap();
        assert_eq!(
            hovered_pack_target(center(row), 800, 600, &rows),
            Some(PackTarget::Row(0))
        );
        toggle_pack(&mut stack, "deleted");
        assert!(stack.is_empty());
    }

    // -- composition ----------------------------------------------------

    #[test]
    fn compose_marks_enabled_disabled_and_missing_rows() {
        let rows = rows(&["beta"], &["alpha"]);
        assert_eq!(
            compose_pack_lines(&rows, None),
            "[x] alpha (missing)\n[ ] beta\nBack"
        );
    }

    #[test]
    fn compose_reports_an_empty_list_then_back() {
        assert_eq!(compose_pack_lines(&[], None), "(no packs found)\nBack");
    }

    #[test]
    fn compose_appends_the_message_last_and_keeps_one_line_per_row() {
        let rows = rows(&["alpha", "beta"], &["beta"]);
        let text = compose_pack_lines(&rows, Some("Pack enabled"));
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines.len(), rows.len() + 2);
        assert_eq!(lines[0], "[x] beta");
        assert_eq!(lines[1], "[ ] alpha");
        assert_eq!(lines[2], "Back");
        assert_eq!(lines[3], "Pack enabled");
        assert!(!text.ends_with('\n'));
    }
}
