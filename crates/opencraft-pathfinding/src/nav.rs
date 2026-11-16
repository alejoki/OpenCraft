//! Pure, deterministic nav-grid A* over a caller-supplied walkability rule.
//!
//! The search is a plain integer-cost A*: unit steps over six neighbors in the
//! fixed order `[-X, +X, -Y, +Y, -Z, +Z]`, no diagonals, no edge weights and no
//! heuristic parameter. Every cell outside [`NavBounds`] is unwalkable and the
//! number of expanded cells is bounded by the caller's budget.
//!
//! The expansion order is a total deterministic order over `(f, h, z, y, x)`,
//! so the same input always yields the same path. Nothing here reads a hash
//! iteration order, a clock or a platform-dependent value.
//!
//! Walkability policy is the caller's adapter: this module has no voxel-query
//! trait of its own. Pass a closure for synthetic grids or a small struct for a
//! real adapter. Nothing here knows an actor, a tick rate, a content identity
//! or a world store.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::error::Error;
use std::fmt;

use glam::IVec3;

/// Fixed 6-neighbor order: one unit along one axis, negative before positive.
///
/// Keeping the order in a constant makes neighbor generation independent of
/// how a caller happens to enumerate directions.
const NEIGHBORS: [IVec3; 6] = [
    IVec3::new(-1, 0, 0),
    IVec3::new(1, 0, 0),
    IVec3::new(0, -1, 0),
    IVec3::new(0, 1, 0),
    IVec3::new(0, 0, -1),
    IVec3::new(0, 0, 1),
];

/// Inclusive axis-aligned cell bounds for one nav grid.
///
/// Both corners are cells inside the volume, so a single-cell volume is
/// `NavBounds::new(pos, pos)`. Bounds are values only: an inverted axis has no
/// cells and never panics.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct NavBounds {
    /// Lowest included cell on each axis.
    pub min: IVec3,
    /// Highest included cell on each axis.
    pub max: IVec3,
}

impl NavBounds {
    /// Create inclusive bounds from two corners.
    pub const fn new(min: IVec3, max: IVec3) -> Self {
        Self { min, max }
    }

    /// Whether `pos` lies inside every inclusive axis range.
    pub fn contains(&self, pos: IVec3) -> bool {
        self.min.x <= pos.x
            && pos.x <= self.max.x
            && self.min.y <= pos.y
            && pos.y <= self.max.y
            && self.min.z <= pos.z
            && pos.z <= self.max.z
    }

    /// Whether any axis is inverted, so the volume holds no cell.
    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z
    }
}

/// One-call walkability probe for a nav grid.
///
/// The blanket closure impl keeps synthetic grids and caller adapters cheap:
///
/// ```
/// use glam::IVec3;
/// use opencraft_pathfinding::NavGrid;
///
/// let floor = |pos: IVec3| pos.y == 0;
/// assert!(floor.is_walkable(IVec3::new(3, 0, -2)));
/// assert!(!floor.is_walkable(IVec3::new(3, 1, -2)));
/// ```
///
/// Cells outside the caller's [`NavBounds`] are never probed by [`find_path`],
/// so an adapter may treat any position as in-range.
pub trait NavGrid {
    /// Whether `pos` may be entered.
    fn is_walkable(&self, pos: IVec3) -> bool;
}

impl<F> NavGrid for F
where
    F: Fn(IVec3) -> bool,
{
    #[inline]
    fn is_walkable(&self, pos: IVec3) -> bool {
        self(pos)
    }
}

/// Why [`find_path`] could not return a route.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PathError {
    /// The start cell is outside [`NavBounds`] or unwalkable.
    StartBlocked,
    /// The goal cell is outside [`NavBounds`] or unwalkable.
    GoalBlocked,
    /// The bounded volume drained before the goal was reached.
    Unreachable,
    /// The expansion budget ran out before the goal was reached.
    NodeLimit,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::StartBlocked => "the start cell is blocked or outside the nav bounds",
            Self::GoalBlocked => "the goal cell is blocked or outside the nav bounds",
            Self::Unreachable => "the goal cannot be reached inside the nav bounds",
            Self::NodeLimit => "the expansion budget ran out before the goal was reached",
        };
        f.write_str(message)
    }
}

impl Error for PathError {}

/// One open-set entry ordered by ascending `(f, h, z, y, x)`.
///
/// The pushed `g` rides along so a popped entry can be skipped when a better
/// cost for its cell is already known; it only breaks exact key ties and never
/// changes which route wins.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct OpenEntry {
    f: u64,
    h: u64,
    pos: IVec3,
    g: u64,
}

impl Ord for OpenEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.f
            .cmp(&other.f)
            .then(self.h.cmp(&other.h))
            .then(self.pos.z.cmp(&other.pos.z))
            .then(self.pos.y.cmp(&other.pos.y))
            .then(self.pos.x.cmp(&other.pos.x))
            .then(self.g.cmp(&other.g))
    }
}

impl PartialOrd for OpenEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Find an inclusive unit-cost path from `start` to `goal`.
///
/// Evaluation order:
///
/// 1. `start` must be inside `bounds` and walkable, else
///    [`PathError::StartBlocked`].
/// 2. `goal` must be inside `bounds` and walkable, else
///    [`PathError::GoalBlocked`].
/// 3. `start == goal` returns `Ok(vec![start])` without expanding anything,
///    even with a `0` budget.
/// 4. Otherwise the bounded search runs.
///
/// The returned path is inclusive: the first cell is `start`, the last cell is
/// `goal`, consecutive cells are exactly one unit apart along one axis, and no
/// cell repeats. Costs are integers only: `g` is the step count from `start`,
/// the heuristic is the Manhattan distance, and `f = g + h` saturates instead
/// of overflowing. There is no weight or heuristic parameter.
///
/// `max_expansions` counts cells that are actually expanded (popped and not
/// stale). A goal popped exactly at the budget boundary still succeeds; a
/// non-goal popped once the budget is spent returns [`PathError::NodeLimit`].
/// A `0` budget therefore only succeeds when `start == goal`. Pass
/// [`usize::MAX`] for "no limit".
///
/// [`PathError::Unreachable`] means the open set drained inside `bounds`
/// without reaching the goal: there is no unbounded fallback. Inverted bounds,
/// out-of-bounds endpoints and blocked cells are ordinary results, never
/// panics.
///
/// ```
/// use glam::IVec3;
/// use opencraft_pathfinding::{find_path, NavBounds, PathError};
///
/// let bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(2, 0, 0));
/// let corridor = |pos: IVec3| bounds.contains(pos);
/// let start = IVec3::new(0, 0, 0);
/// let goal = IVec3::new(2, 0, 0);
///
/// assert_eq!(
///     find_path(&corridor, bounds, start, goal, usize::MAX),
///     Ok(vec![start, IVec3::new(1, 0, 0), goal]),
/// );
/// assert_eq!(
///     find_path(&corridor, bounds, start, goal, 0),
///     Err(PathError::NodeLimit),
/// );
/// ```
pub fn find_path<G: NavGrid + ?Sized>(
    grid: &G,
    bounds: NavBounds,
    start: IVec3,
    goal: IVec3,
    max_expansions: usize,
) -> Result<Vec<IVec3>, PathError> {
    if !bounds.contains(start) || !grid.is_walkable(start) {
        return Err(PathError::StartBlocked);
    }
    if !bounds.contains(goal) || !grid.is_walkable(goal) {
        return Err(PathError::GoalBlocked);
    }
    if start == goal {
        return Ok(vec![start]);
    }

    let mut best_g: HashMap<(i32, i32, i32), u64> = HashMap::new();
    let mut came_from: HashMap<(i32, i32, i32), (i32, i32, i32)> = HashMap::new();
    let mut open: BinaryHeap<Reverse<OpenEntry>> = BinaryHeap::new();

    let start_h = manhattan(start, goal);
    best_g.insert(cell(start), 0);
    open.push(Reverse(OpenEntry {
        f: start_h,
        h: start_h,
        pos: start,
        g: 0,
    }));

    let mut expansions: usize = 0;

    while let Some(Reverse(entry)) = open.pop() {
        let pos = entry.pos;

        // A cheaper route to this cell was found after this entry was pushed.
        // Skipped duplicates are not expansions.
        match best_g.get(&cell(pos)) {
            Some(&known) if known == entry.g => {}
            _ => continue,
        }

        // The goal is tested on pop, before the budget check, so a goal reached
        // exactly at the budget boundary still succeeds.
        if pos == goal {
            return Ok(reconstruct_path(&came_from, start, goal));
        }

        if expansions >= max_expansions {
            return Err(PathError::NodeLimit);
        }
        expansions += 1;

        let next_g = entry.g.saturating_add(1);
        for offset in NEIGHBORS {
            let next = pos + offset;
            if !bounds.contains(next) || !grid.is_walkable(next) {
                continue;
            }
            let next_cell = cell(next);
            if best_g.get(&next_cell).is_some_and(|&known| known <= next_g) {
                continue;
            }
            best_g.insert(next_cell, next_g);
            came_from.insert(next_cell, cell(pos));
            let h = manhattan(next, goal);
            open.push(Reverse(OpenEntry {
                f: next_g.saturating_add(h),
                h,
                pos: next,
                g: next_g,
            }));
        }
    }

    Err(PathError::Unreachable)
}

/// Manhattan distance between two cells as an unsigned step count.
#[inline]
fn manhattan(a: IVec3, b: IVec3) -> u64 {
    let delta = a - b;
    u64::from(delta.x.unsigned_abs())
        + u64::from(delta.y.unsigned_abs())
        + u64::from(delta.z.unsigned_abs())
}

/// Hash key for one cell; keeps the maps lookup-only so iteration order can
/// never influence the result.
#[inline]
fn cell(pos: IVec3) -> (i32, i32, i32) {
    (pos.x, pos.y, pos.z)
}

/// Walk the came-from chain from `goal` back to `start`, then reverse it.
fn reconstruct_path(
    came_from: &HashMap<(i32, i32, i32), (i32, i32, i32)>,
    start: IVec3,
    goal: IVec3,
) -> Vec<IVec3> {
    let mut path = vec![goal];
    let mut current = goal;
    while current != start {
        let previous = came_from[&cell(current)];
        current = IVec3::new(previous.0, previous.1, previous.2);
        path.push(current);
    }
    path.reverse();
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_corridor_returns_the_inclusive_optimal_path() {
        let bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(4, 0, 0));
        let open = |pos: IVec3| bounds.contains(pos);
        let start = IVec3::new(0, 0, 0);
        let goal = IVec3::new(4, 0, 0);
        let expected = vec![
            IVec3::new(0, 0, 0),
            IVec3::new(1, 0, 0),
            IVec3::new(2, 0, 0),
            IVec3::new(3, 0, 0),
            IVec3::new(4, 0, 0),
        ];

        assert_eq!(
            find_path(&open, bounds, start, goal, usize::MAX),
            Ok(expected.clone())
        );
        // Four interior expansions reach the goal exactly at the boundary.
        assert_eq!(find_path(&open, bounds, start, goal, 4), Ok(expected));
        assert_eq!(
            find_path(&open, bounds, start, goal, 3),
            Err(PathError::NodeLimit)
        );
        assert_eq!(
            find_path(&open, bounds, start, goal, 0),
            Err(PathError::NodeLimit)
        );
    }

    #[test]
    fn detour_walks_around_a_wall_and_stays_optimal() {
        let bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(4, 0, 4));
        let walled = |pos: IVec3| bounds.contains(pos) && !(pos.x == 2 && pos.z != 2);
        let start = IVec3::new(0, 0, 0);
        let goal = IVec3::new(4, 0, 0);
        let expected = vec![
            IVec3::new(0, 0, 0),
            IVec3::new(1, 0, 0),
            IVec3::new(1, 0, 1),
            IVec3::new(1, 0, 2),
            IVec3::new(2, 0, 2),
            IVec3::new(3, 0, 2),
            IVec3::new(3, 0, 1),
            IVec3::new(3, 0, 0),
            IVec3::new(4, 0, 0),
        ];

        let path = find_path(&walled, bounds, start, goal, usize::MAX).expect("detour path");
        assert_eq!(path, expected);
        // 9 cells = 8 steps: the Manhattan lower bound 4 plus a 4-step detour.
        assert_eq!(path.len() - 1, 8);
        assert_eq!(manhattan(start, goal), 4);
    }

    #[test]
    fn enclosed_goal_reports_unreachable_and_respects_the_budget() {
        let bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(2, 1, 2));
        // The goal is sealed on all six sides: four horizontal walls, the floor
        // outside `bounds`, and the ceiling cell above it.
        let sealed = |pos: IVec3| {
            bounds.contains(pos)
                && !matches!(
                    (pos.x, pos.y, pos.z),
                    (0, 0, 1) | (2, 0, 1) | (1, 0, 0) | (1, 0, 2) | (1, 1, 1)
                )
        };
        let start = IVec3::new(0, 0, 0);
        let goal = IVec3::new(1, 0, 1);

        assert_eq!(
            find_path(&sealed, bounds, start, goal, usize::MAX),
            Err(PathError::Unreachable)
        );
        assert_eq!(
            find_path(&sealed, bounds, start, goal, 1),
            Err(PathError::NodeLimit)
        );
    }

    #[test]
    fn blocked_or_out_of_bounds_endpoints_report_their_own_error() {
        let bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(2, 0, 2));
        let blocked = IVec3::new(1, 0, 1);
        let grid = |pos: IVec3| bounds.contains(pos) && pos != blocked;
        let open = IVec3::new(0, 0, 0);
        let far = IVec3::new(2, 0, 2);

        assert_eq!(
            find_path(&grid, bounds, blocked, open, usize::MAX),
            Err(PathError::StartBlocked)
        );
        assert_eq!(
            find_path(&grid, bounds, open, blocked, usize::MAX),
            Err(PathError::GoalBlocked)
        );
        assert_eq!(
            find_path(&grid, bounds, blocked, blocked, usize::MAX),
            Err(PathError::StartBlocked)
        );

        let outside_start = IVec3::new(-1, 0, 0);
        assert_eq!(
            find_path(&grid, bounds, outside_start, far, usize::MAX),
            Err(PathError::StartBlocked)
        );
        let outside_goal = IVec3::new(3, 0, 2);
        assert_eq!(
            find_path(&grid, bounds, open, outside_goal, usize::MAX),
            Err(PathError::GoalBlocked)
        );

        let inverted = NavBounds::new(far, IVec3::new(0, 0, 0));
        assert!(inverted.is_empty());
        assert_eq!(
            find_path(&grid, inverted, open, far, usize::MAX),
            Err(PathError::StartBlocked)
        );
    }

    #[test]
    fn start_equals_goal_returns_one_cell_without_expanding() {
        let bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(1, 1, 1));
        let open = |pos: IVec3| bounds.contains(pos);
        let start = IVec3::new(1, 1, 1);

        assert_eq!(find_path(&open, bounds, start, start, 0), Ok(vec![start]));

        // Every neighbor is unwalkable, but no neighbor is ever generated.
        let isolated = |pos: IVec3| pos == start;
        assert_eq!(
            find_path(&isolated, bounds, start, start, 0),
            Ok(vec![start])
        );
    }

    #[test]
    fn tie_break_is_total_and_reproducible() {
        let cube = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(1, 1, 1));
        let open_cube = |pos: IVec3| cube.contains(pos);
        let cube_start = IVec3::new(0, 0, 0);
        let cube_goal = IVec3::new(1, 1, 1);
        // Pins y before z: the route climbs at x = 1 and z = 0 rather than
        // stepping through z = 1 first.
        assert_eq!(
            find_path(&open_cube, cube, cube_start, cube_goal, usize::MAX),
            Ok(vec![
                IVec3::new(0, 0, 0),
                IVec3::new(1, 0, 0),
                IVec3::new(1, 1, 0),
                IVec3::new(1, 1, 1),
            ])
        );

        let plane = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(4, 4, 0));
        let open_plane = |pos: IVec3| plane.contains(pos);
        let plane_start = IVec3::new(0, 0, 0);
        let plane_goal = IVec3::new(4, 4, 0);
        let expected_plane = vec![
            IVec3::new(0, 0, 0),
            IVec3::new(1, 0, 0),
            IVec3::new(2, 0, 0),
            IVec3::new(3, 0, 0),
            IVec3::new(4, 0, 0),
            IVec3::new(4, 1, 0),
            IVec3::new(4, 2, 0),
            IVec3::new(4, 3, 0),
            IVec3::new(4, 4, 0),
        ];
        // Pins h before coordinates: lower remaining distance wins the tie.
        assert_eq!(
            find_path(&open_plane, plane, plane_start, plane_goal, usize::MAX),
            Ok(expected_plane)
        );

        let detour_bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(4, 0, 4));
        let detour = |pos: IVec3| detour_bounds.contains(pos) && !(pos.x == 2 && pos.z != 2);
        let first = find_path(
            &detour,
            detour_bounds,
            IVec3::new(0, 0, 0),
            IVec3::new(4, 0, 0),
            usize::MAX,
        )
        .expect("detour path");
        for _ in 0..7 {
            assert_eq!(
                find_path(
                    &detour,
                    detour_bounds,
                    IVec3::new(0, 0, 0),
                    IVec3::new(4, 0, 0),
                    usize::MAX,
                ),
                Ok(first.clone())
            );
        }
    }

    #[test]
    fn nav_bounds_report_inclusive_containment_and_emptiness() {
        let bounds = NavBounds::new(IVec3::new(-2, 0, 3), IVec3::new(2, 4, 5));

        assert!(bounds.contains(bounds.min));
        assert!(bounds.contains(bounds.max));
        assert!(bounds.contains(IVec3::new(0, 2, 4)));

        // One cell past each corner is outside.
        assert!(!bounds.contains(IVec3::new(-3, 2, 4)));
        assert!(!bounds.contains(IVec3::new(3, 2, 4)));
        assert!(!bounds.contains(IVec3::new(0, -1, 4)));
        assert!(!bounds.contains(IVec3::new(0, 5, 4)));
        assert!(!bounds.contains(IVec3::new(0, 2, 2)));
        assert!(!bounds.contains(IVec3::new(0, 2, 6)));

        assert!(!bounds.is_empty());
        assert!(NavBounds::new(IVec3::new(1, 0, 0), IVec3::new(0, 4, 4)).is_empty());
        assert!(NavBounds::new(IVec3::new(0, 1, 0), IVec3::new(2, 0, 2)).is_empty());
        assert!(NavBounds::new(IVec3::new(0, 0, 1), IVec3::new(2, 2, 0)).is_empty());

        let single = NavBounds::new(IVec3::new(1, 2, 3), IVec3::new(1, 2, 3));
        assert!(!single.is_empty());
        assert!(single.contains(IVec3::new(1, 2, 3)));
        assert!(!single.contains(IVec3::new(2, 2, 3)));
    }
}
