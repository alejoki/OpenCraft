//! External public-API contract for the nav-grid A* solver.
//!
//! Every import comes from the crate root on purpose: this target proves the
//! `find_path` / `NavBounds` / `NavGrid` / `PathError` re-exports are usable by
//! a consumer crate that never reaches into `opencraft_pathfinding::nav`.

use std::collections::HashSet;

use glam::IVec3;
use opencraft_pathfinding::{find_path, NavBounds, NavGrid, PathError};

/// Manhattan step count between two cells.
fn manhattan(a: IVec3, b: IVec3) -> i32 {
    let delta = a - b;
    delta.x.abs() + delta.y.abs() + delta.z.abs()
}

/// Shared shape checks: inclusive endpoints, unit 6-neighbor steps, in-bounds
/// walkable cells only, and no repeated cell.
fn assert_path_invariants<G: NavGrid + ?Sized>(
    grid: &G,
    bounds: NavBounds,
    start: IVec3,
    goal: IVec3,
    path: &[IVec3],
) {
    assert!(!path.is_empty(), "a path always contains the start cell");
    assert_eq!(path.first(), Some(&start), "path must start at start");
    assert_eq!(path.last(), Some(&goal), "path must end at goal");

    let mut seen = HashSet::new();
    for &cell in path {
        assert!(seen.insert(cell), "path repeats {cell:?}");
        assert!(bounds.contains(cell), "{cell:?} escapes {bounds:?}");
        assert!(grid.is_walkable(cell), "{cell:?} is not walkable");
    }

    for step in path.windows(2) {
        assert_eq!(
            manhattan(step[0], step[1]),
            1,
            "step {:?} -> {:?} is not a unit 6-neighbor move",
            step[0],
            step[1]
        );
    }
}

#[test]
fn closure_grid_satisfies_the_trait_and_finds_a_path() {
    let bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(4, 0, 0));
    // The closure is handed straight to `find_path`, which relies on the
    // blanket `NavGrid` impl for `Fn(IVec3) -> bool`.
    let corridor = |pos: IVec3| pos.y == 0 && pos.z == 0 && (0..=4).contains(&pos.x);
    let start = IVec3::new(0, 0, 0);
    let goal = IVec3::new(4, 0, 0);

    assert_eq!(
        find_path(&corridor, bounds, start, goal, usize::MAX),
        Ok(vec![
            IVec3::new(0, 0, 0),
            IVec3::new(1, 0, 0),
            IVec3::new(2, 0, 0),
            IVec3::new(3, 0, 0),
            IVec3::new(4, 0, 0),
        ])
    );
}

#[test]
fn custom_struct_grid_implements_the_trait() {
    /// An adapter outside the crate: only recorded pillar cells are unwalkable.
    struct Pillars(HashSet<IVec3>);

    impl NavGrid for Pillars {
        fn is_walkable(&self, pos: IVec3) -> bool {
            !self.0.contains(&pos)
        }
    }

    let bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(4, 0, 4));
    let pillars = Pillars(HashSet::from([IVec3::new(1, 0, 0), IVec3::new(3, 0, 0)]));
    let start = IVec3::new(0, 0, 0);
    let goal = IVec3::new(4, 0, 0);

    let path =
        find_path(&pillars, bounds, start, goal, usize::MAX).expect("path around the pillars");

    assert_path_invariants(&pillars, bounds, start, goal, &path);
    // Manhattan distance 4 plus one step out of the z = 0 lane and one back.
    assert_eq!(path.len() - 1, 6);
}

#[test]
fn path_invariants_hold_across_synthetic_worlds() {
    // 1. Open box with the endpoints on opposite corners.
    let open_bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(4, 2, 4));
    let open = |pos: IVec3| open_bounds.contains(pos);
    let start = IVec3::new(0, 0, 0);
    let goal = IVec3::new(4, 2, 4);
    let path = find_path(&open, open_bounds, start, goal, usize::MAX).expect("open box path");
    assert_path_invariants(&open, open_bounds, start, goal, &path);
    assert_eq!(path.len() as i32 - 1, manhattan(start, goal));

    // 2. Wall at x == 2 with a single gap at z == 2.
    let wall_bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(4, 0, 4));
    let walled = |pos: IVec3| wall_bounds.contains(pos) && !(pos.x == 2 && pos.z != 2);
    let wall_start = IVec3::new(0, 0, 0);
    let wall_goal = IVec3::new(4, 0, 0);
    let path = find_path(&walled, wall_bounds, wall_start, wall_goal, usize::MAX)
        .expect("path through the gap");
    assert_path_invariants(&walled, wall_bounds, wall_start, wall_goal, &path);

    // 3. Deterministic pseudorandom pillar maze: no clock, no RNG crate.
    let maze_bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(5, 1, 5));
    let maze_start = IVec3::new(0, 0, 0);
    let maze_goal = IVec3::new(5, 0, 5);
    let maze = |pos: IVec3| {
        pos == maze_start
            || pos == maze_goal
            || (pos.x * 31 + pos.y * 7 + pos.z * 17).rem_euclid(11) >= 3
    };
    let path = find_path(&maze, maze_bounds, maze_start, maze_goal, usize::MAX).expect("maze path");
    assert_path_invariants(&maze, maze_bounds, maze_start, maze_goal, &path);

    // 4. A sealed goal is an error, so the invariant helper does not apply.
    let sealed_bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(2, 1, 2));
    let sealed = |pos: IVec3| {
        sealed_bounds.contains(pos)
            && !matches!(
                (pos.x, pos.y, pos.z),
                (0, 0, 1) | (2, 0, 1) | (1, 0, 0) | (1, 0, 2) | (1, 1, 1)
            )
    };
    assert_eq!(
        find_path(
            &sealed,
            sealed_bounds,
            IVec3::new(0, 0, 0),
            IVec3::new(1, 0, 1),
            usize::MAX
        ),
        Err(PathError::Unreachable)
    );
}

#[test]
fn search_is_reproducible_across_calls() {
    let plane_bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(4, 4, 0));
    let plane = |pos: IVec3| plane_bounds.contains(pos);
    let plane_start = IVec3::new(0, 0, 0);
    let plane_goal = IVec3::new(4, 4, 0);
    let expected = find_path(&plane, plane_bounds, plane_start, plane_goal, usize::MAX)
        .expect("open plane path");
    for _ in 0..16 {
        assert_eq!(
            find_path(&plane, plane_bounds, plane_start, plane_goal, usize::MAX),
            Ok(expected.clone())
        );
    }

    let detour_bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(4, 0, 4));
    let detour = |pos: IVec3| detour_bounds.contains(pos) && !(pos.x == 2 && pos.z != 2);
    let detour_goal = IVec3::new(4, 0, 0);
    let expected = find_path(&detour, detour_bounds, plane_start, detour_goal, usize::MAX)
        .expect("detour path");
    for _ in 0..16 {
        assert_eq!(
            find_path(&detour, detour_bounds, plane_start, detour_goal, usize::MAX),
            Ok(expected.clone())
        );
    }
}

#[test]
fn path_length_matches_the_manhattan_lower_bound_when_unobstructed() {
    let bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(4, 0, 4));
    let open = |pos: IVec3| bounds.contains(pos);
    let start = IVec3::new(0, 0, 0);
    let goal = IVec3::new(4, 0, 4);

    let path = find_path(&open, bounds, start, goal, usize::MAX).expect("open path");

    assert_eq!(path.len(), 9);
    assert_eq!(path.len() as i32 - 1, manhattan(start, goal));
    for step in path.windows(2) {
        assert_eq!(manhattan(step[1], goal), manhattan(step[0], goal) - 1);
    }
}

#[test]
fn errors_are_typed_and_displayable() {
    let open_bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(2, 0, 2));
    let blocked_cell = IVec3::new(1, 0, 1);
    let open = IVec3::new(0, 0, 0);
    let far = IVec3::new(2, 0, 2);
    let grid = |pos: IVec3| open_bounds.contains(pos) && pos != blocked_cell;

    let start_blocked = find_path(&grid, open_bounds, blocked_cell, open, usize::MAX)
        .expect_err("blocked start must fail");
    assert_eq!(start_blocked, PathError::StartBlocked);

    let goal_blocked = find_path(&grid, open_bounds, open, blocked_cell, usize::MAX)
        .expect_err("blocked goal must fail");
    assert_eq!(goal_blocked, PathError::GoalBlocked);

    let sealed_bounds = NavBounds::new(IVec3::new(0, 0, 0), IVec3::new(2, 1, 2));
    let sealed = |pos: IVec3| {
        sealed_bounds.contains(pos)
            && !matches!(
                (pos.x, pos.y, pos.z),
                (0, 0, 1) | (2, 0, 1) | (1, 0, 0) | (1, 0, 2) | (1, 1, 1)
            )
    };
    let unreachable = find_path(&sealed, sealed_bounds, open, blocked_cell, usize::MAX)
        .expect_err("sealed goal must fail");
    assert_eq!(unreachable, PathError::Unreachable);

    let node_limit = find_path(&grid, open_bounds, open, far, 0).expect_err("spent budget");
    assert_eq!(node_limit, PathError::NodeLimit);

    for err in [start_blocked, goal_blocked, unreachable, node_limit] {
        let rendered = format!("{err}");
        assert!(!rendered.is_empty(), "{err:?} must have a display message");
        assert_eq!(err.to_string(), rendered);

        let dynamic: &dyn std::error::Error = &err;
        assert_eq!(dynamic.to_string(), rendered);
    }
}

#[test]
fn unbounded_budget_drains_to_unreachable() {
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
