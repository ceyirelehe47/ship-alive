//! Grid A* pathfinding, 8-directional with geometric costs.
//!
//! - Cardinal step cost `10`, diagonal step cost `14` (≈ 10·√2) — integers
//!   keep the search deterministic and overflow-free.
//! - Heuristic: octile distance in the same cost scale, admissible and
//!   consistent for the 10/14 cost model.
//! - Corner cutting is strictly forbidden: a diagonal step requires BOTH of
//!   its side cells to be free (map walls and dynamic blockers alike), so
//!   crew can never squeeze through wall corners or between two blockers.
//!
//! Kept as pure functions over `ShipMap` so they can be tested standalone.
//! `blocked` lets callers treat dynamic obstacles (e.g. crew standing in a
//! corridor) as walls when re-planning around congestion.

use crate::map::{ShipMap, TilePos};
use std::collections::BinaryHeap;

/// Fixed-point step costs (×10 tile-distance). 14 ≈ 10·√2, slightly under
/// the true value so the heuristic stays admissible.
pub const COST_CARDINAL: u32 = 10;
pub const COST_DIAGONAL: u32 = 14;

/// World-space length of one step (1.0 cardinal, √2 diagonal).
pub fn step_length(a: TilePos, b: TilePos) -> f32 {
    if a.x != b.x && a.y != b.y {
        std::f32::consts::SQRT_2
    } else {
        1.0
    }
}

/// Fixed-point step cost between 4/8-adjacent tiles.
pub fn step_cost(a: TilePos, b: TilePos) -> u32 {
    if a.x != b.x && a.y != b.y {
        COST_DIAGONAL
    } else {
        COST_CARDINAL
    }
}

/// Total world-space length of a path (Σ step lengths). The optional `from`
/// accounts for the step from the walker's current tile to `path[0]`.
pub fn path_length(from: Option<TilePos>, path: &[TilePos]) -> f32 {
    let mut total = 0.0;
    let mut prev = from;
    for t in path {
        if let Some(p) = prev {
            total += step_length(p, *t);
        }
        prev = Some(*t);
    }
    total
}

/// Total fixed-point cost of a path (Σ step costs), optional `from` as above.
pub fn path_cost(from: Option<TilePos>, path: &[TilePos]) -> u32 {
    let mut total = 0;
    let mut prev = from;
    for t in path {
        if let Some(p) = prev {
            total += step_cost(p, *t);
        }
        prev = Some(*t);
    }
    total
}

/// Octile distance in fixed-point cost scale — the 8-way admissible
/// heuristic: `10·max(dx,dy) + 4·min(dx,dy)`.
pub fn octile_cost(a: TilePos, b: TilePos) -> u32 {
    let dx = (a.x - b.x).unsigned_abs();
    let dy = (a.y - b.y).unsigned_abs();
    COST_CARDINAL * dx.max(dy) + (COST_DIAGONAL - COST_CARDINAL) * dx.min(dy)
}

/// Approximate world-space octile distance (for job-distance estimates and
/// telemetry where the √2 geometry should not be flattened to Manhattan).
pub fn octile_distance(a: TilePos, b: TilePos) -> f32 {
    let dx = (a.x - b.x).unsigned_abs() as f32;
    let dy = (a.y - b.y).unsigned_abs() as f32;
    dx.max(dy) + (std::f32::consts::SQRT_2 - 1.0) * dx.min(dy)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct OpenNode {
    cost: u32,
    est_total: u32,
    pos: TilePos,
}

impl Ord for OpenNode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // BinaryHeap is a max-heap: order by estimated total, then lower cost,
        // so the most promising node pops first.
        other
            .est_total
            .cmp(&self.est_total)
            .then(other.cost.cmp(&self.cost))
    }
}

impl PartialOrd for OpenNode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The 8 move directions: cardinals first so equal-cost paths prefer
/// straight lines over diagonal zig-zags (stable, deterministic).
const DIRS: [(i32, i32); 8] = [
    (1, 0),
    (-1, 0),
    (0, 1),
    (0, -1),
    (1, 1),
    (1, -1),
    (-1, 1),
    (-1, -1),
];

/// Can the walker at `from` enter `to`? For diagonal steps BOTH side cells
/// must be walkable AND unblocked — strict no-corner-cutting.
fn step_enterable(
    map: &ShipMap,
    from: TilePos,
    to: TilePos,
    blocked: &impl Fn(TilePos) -> bool,
) -> bool {
    if !map.is_walkable(to) || blocked(to) {
        return false;
    }
    if from.x != to.x && from.y != to.y {
        let side_a = TilePos::new(to.x, from.y);
        let side_b = TilePos::new(from.x, to.y);
        if !map.is_walkable(side_a)
            || blocked(side_a)
            || !map.is_walkable(side_b)
            || blocked(side_b)
        {
            return false;
        }
    }
    true
}

/// Returns the path from `from` to `to` excluding `from` itself.
/// Returns `None` if no route exists (or the goal is not enterable).
///
/// Search state lives in flat `u32` arrays indexed by tile index (the map is
/// a dense grid) instead of hash maps — this is the inner loop of every job
/// claim. Expansion order (heap, DIRS order, relax check) is unchanged, so
/// the produced paths are identical to the previous HashMap version.
pub fn find_path(
    map: &ShipMap,
    from: TilePos,
    to: TilePos,
    blocked: impl Fn(TilePos) -> bool,
) -> Option<Vec<TilePos>> {
    if from == to {
        return Some(Vec::new());
    }
    // The goal must be enterable (a locked door is a wall); the start only
    // standable — a crew member caught inside a door tile when it locks must
    // still be able to path *out* of it.
    if !map.is_walkable(to) || !map.is_standable(from) {
        return None;
    }
    let heuristic = |p: TilePos| octile_cost(p, to);
    let (w, n) = (map.width as u32, map.width as u32 * map.height as u32);
    let idx = |p: TilePos| (p.y as u32 * w + p.x as u32) as usize;
    let mut open = BinaryHeap::new();
    let mut best_cost = vec![u32::MAX; n as usize];
    let mut came_from = vec![u32::MAX; n as usize];
    let from_i = idx(from);

    open.push(OpenNode {
        cost: 0,
        est_total: heuristic(from),
        pos: from,
    });
    best_cost[from_i] = 0;

    while let Some(OpenNode { cost, pos, .. }) = open.pop() {
        if pos == to {
            let mut path = vec![to];
            let mut cur = idx(to) as u32;
            loop {
                let prev_i = came_from[cur as usize];
                if prev_i == u32::MAX || prev_i == from_i as u32 {
                    break;
                }
                path.push(TilePos::new((prev_i % w) as i32, (prev_i / w) as i32));
                cur = prev_i;
            }
            path.reverse();
            return Some(path);
        }
        // Stale queue entry (a better route to `pos` was already expanded).
        let pos_i = idx(pos);
        if best_cost[pos_i] < cost {
            continue;
        }
        for (dx, dy) in DIRS {
            let next = TilePos::new(pos.x + dx, pos.y + dy);
            if !step_enterable(map, pos, next, &blocked) {
                continue;
            }
            let next_cost = cost + step_cost(pos, next);
            let next_i = idx(next);
            if next_cost < best_cost[next_i] {
                best_cost[next_i] = next_cost;
                came_from[next_i] = pos_i as u32;
                open.push(OpenNode {
                    cost: next_cost,
                    est_total: next_cost + heuristic(next),
                    pos: next,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn map(rows: &[&str]) -> ShipMap {
        ShipMap::from_layout(rows).0
    }

    fn path_is_legal(map: &ShipMap, from: TilePos, path: &[TilePos]) {
        let mut prev = from;
        for t in path {
            assert!(
                step_enterable(map, prev, *t, &|_| false),
                "illegal step {prev:?}->{t:?}"
            );
            prev = *t;
        }
    }

    // A — open diagonal: empty field, S top-left, G bottom-right.
    #[test]
    fn open_field_prefers_diagonal() {
        let m = map(&["....", "....", "....", "...."]);
        let p = find_path(&m, TilePos::new(0, 0), TilePos::new(3, 3), |_| false).unwrap();
        assert_eq!(p.len(), 3, "3 diagonal steps, got {p:?}");
        let mut prev = TilePos::new(0, 0);
        for t in &p {
            assert!(
                prev.x != t.x && prev.y != t.y,
                "step {prev:?}->{t:?} not diagonal"
            );
            prev = *t;
        }
        assert_eq!(path_cost(Some(TilePos::new(0, 0)), &p), 3 * COST_DIAGONAL);
    }

    // B — weighted cost: diagonal route costs ~√2 per step, not 1.
    #[test]
    fn diagonal_costs_root_two() {
        assert_eq!(
            step_cost(TilePos::new(0, 0), TilePos::new(1, 0)),
            COST_CARDINAL
        );
        assert_eq!(
            step_cost(TilePos::new(0, 0), TilePos::new(1, 1)),
            COST_DIAGONAL
        );
        assert!((COST_DIAGONAL as f32) > COST_CARDINAL as f32 * 1.3);
        assert!((COST_DIAGONAL as f32) < COST_CARDINAL as f32 * 1.5);
        assert_eq!(
            step_length(TilePos::new(0, 0), TilePos::new(1, 1)),
            std::f32::consts::SQRT_2
        );
        assert_eq!(
            path_length(Some(TilePos::new(0, 0)), &[TilePos::new(1, 1)]),
            std::f32::consts::SQRT_2
        );
    }

    // C — octile optimality against a brute-force Dijkstra on obstacle maps.
    #[test]
    fn matches_reference_dijkstra() {
        let layouts: Vec<Vec<&str>> = vec![
            vec![
                "#######", "#.....#", "#.###.#", "#.#...#", "#...#.#", "#######",
            ],
            vec!["########", "#..#...#", "#..#.#.#", "#....#.#", "########"],
            vec!["########", "#......#", "#.####.#", "#......#", "########"],
        ];
        for rows in &layouts {
            let m = map(rows);
            let start = TilePos::new(1, 1);
            for y in 0..m.height {
                for x in 0..m.width {
                    let goal = TilePos::new(x, y);
                    if !m.is_walkable(goal) {
                        continue;
                    }
                    let got = find_path(&m, start, goal, |_| false);
                    let want = dijkstra_cost(&m, start, goal);
                    match (&got, want) {
                        (Some(p), Some(w)) => {
                            assert_eq!(
                                path_cost(Some(start), p),
                                w,
                                "cost mismatch to {goal:?} in {rows:?}"
                            );
                            path_is_legal(&m, start, p);
                        }
                        (None, None) => {}
                        (Some(_), None) | (None, Some(_)) => {
                            panic!("reachability mismatch at {goal:?}")
                        }
                    }
                }
            }
        }
    }

    /// Tiny reference Dijkstra with the same cost + corner rules.
    fn dijkstra_cost(m: &ShipMap, from: TilePos, to: TilePos) -> Option<u32> {
        #[derive(PartialEq, Eq)]
        struct N(u32, TilePos);
        impl Ord for N {
            fn cmp(&self, o: &Self) -> std::cmp::Ordering {
                o.0.cmp(&self.0)
            }
        }
        impl PartialOrd for N {
            fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(o))
            }
        }
        let mut dist: HashMap<TilePos, u32> = HashMap::from([(from, 0)]);
        let mut heap = BinaryHeap::from([N(0, from)]);
        while let Some(N(d, p)) = heap.pop() {
            if p == to {
                return Some(d);
            }
            if dist.get(&p).is_some_and(|c| *c < d) {
                continue;
            }
            for (dx, dy) in DIRS {
                let n = TilePos::new(p.x + dx, p.y + dy);
                if !step_enterable(m, p, n, &|_| false) {
                    continue;
                }
                let nd = d + step_cost(p, n);
                if nd < *dist.get(&n).unwrap_or(&u32::MAX) {
                    dist.insert(n, nd);
                    heap.push(N(nd, n));
                }
            }
        }
        None
    }

    // D — double-wall corner: no diagonal through two walls.
    #[test]
    fn double_wall_corner_forbidden() {
        let m = map(&[".#", "#."]);
        assert!(find_path(&m, TilePos::new(0, 0), TilePos::new(1, 1), |_| false).is_none());
    }

    // E — single-wall corner: still forbidden (strict rule). A path exists
    // but it must pay the full detour (2 cardinal steps), never the diagonal.
    #[test]
    fn single_wall_corner_forbidden() {
        let m = map(&[".#", ".."]);
        let p = find_path(&m, TilePos::new(0, 0), TilePos::new(1, 1), |_| false)
            .expect("route around the corner exists");
        assert_eq!(
            path_cost(Some(TilePos::new(0, 0)), &p),
            2 * COST_CARDINAL,
            "strict rule: one blocked side cell forbids the diagonal, got {p:?}"
        );
    }

    // F — open corner: diagonal allowed when both sides are free.
    #[test]
    fn open_corner_diagonal_allowed() {
        let m = map(&["..", ".."]);
        let p = find_path(&m, TilePos::new(0, 0), TilePos::new(1, 1), |_| false).unwrap();
        assert_eq!(p, vec![TilePos::new(1, 1)]);
    }

    // G — dynamic blockers also guard the side cells.
    #[test]
    fn dynamic_blocker_guards_diagonal_sides() {
        let m = map(&["...", "...", "..."]);
        // Block the east side cell of the (0,0)->(1,1) diagonal.
        let blocked = |t: TilePos| t == TilePos::new(1, 0);
        let p = find_path(&m, TilePos::new(0, 0), TilePos::new(2, 2), blocked).unwrap();
        let mut prev = TilePos::new(0, 0);
        for t in &p {
            assert!(
                step_enterable(&m, prev, *t, &blocked),
                "illegal step {prev:?}->{t:?} around dynamic blocker"
            );
            prev = *t;
        }
        // The blocked tile itself is never enterable.
        assert!(find_path(&m, TilePos::new(0, 0), TilePos::new(1, 0), blocked).is_none());
    }

    #[test]
    fn straight_line_stays_cardinal() {
        let m = map(&["#####", "#...#", "#.#.#", "#...#", "#####"]);
        let p = find_path(&m, TilePos::new(1, 1), TilePos::new(3, 1), |_| false).unwrap();
        assert_eq!(p, vec![TilePos::new(2, 1), TilePos::new(3, 1)]);
    }

    #[test]
    fn detours_around_wall() {
        let m = map(&["#####", "#...#", "#.#.#", "#...#", "#####"]);
        let p = find_path(&m, TilePos::new(1, 2), TilePos::new(3, 2), |_| false).unwrap();
        assert_eq!(p.len(), 4);
        assert_eq!(*p.last().unwrap(), TilePos::new(3, 2));
    }

    #[test]
    fn goal_blocked_by_dynamic_obstacle() {
        let m = map(&["#####", "#...#", "#.#.#", "#...#", "#####"]);
        assert!(find_path(&m, TilePos::new(1, 1), TilePos::new(3, 1), |p| p
            == TilePos::new(3, 1))
        .is_none());
    }

    #[test]
    fn zero_length_path_when_already_there() {
        let m = map(&["#####", "#...#", "#.#.#", "#...#", "#####"]);
        assert!(
            find_path(&m, TilePos::new(2, 1), TilePos::new(2, 1), |_| false)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn octile_helpers() {
        let a = TilePos::new(0, 0);
        assert_eq!(octile_cost(a, TilePos::new(3, 0)), 30);
        assert_eq!(octile_cost(a, TilePos::new(3, 3)), 3 * 10 + 3 * 4);
        assert!(
            (octile_distance(a, TilePos::new(3, 3)) - 3.0 * std::f32::consts::SQRT_2).abs() < 1e-4
        );
    }
}
