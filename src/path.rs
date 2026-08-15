//! Grid A* pathfinding, 4-directional.
//!
//! Kept as a pure function over `ShipMap` so it can be tested standalone.
//! `blocked` lets callers treat dynamic obstacles (e.g. crew standing in a
//! corridor) as walls when re-planning around congestion.

use crate::map::{ShipMap, TilePos};
use std::collections::{BinaryHeap, HashMap};

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

/// Returns the path from `from` to `to` excluding `from` itself.
/// Returns `None` if no route exists (or the goal is not walkable).
pub fn find_path(
    map: &ShipMap,
    from: TilePos,
    to: TilePos,
    blocked: impl Fn(TilePos) -> bool,
) -> Option<Vec<TilePos>> {
    if from == to {
        return Some(Vec::new());
    }
    if !map.is_walkable(to) || !map.is_walkable(from) {
        return None;
    }
    let heuristic = |p: TilePos| (p.x - to.x).unsigned_abs() + (p.y - to.y).unsigned_abs();
    let mut open = BinaryHeap::new();
    let mut best_cost: HashMap<TilePos, u32> = HashMap::new();
    let mut came_from: HashMap<TilePos, TilePos> = HashMap::new();

    open.push(OpenNode {
        cost: 0,
        est_total: heuristic(from),
        pos: from,
    });
    best_cost.insert(from, 0);

    while let Some(OpenNode { cost, pos, .. }) = open.pop() {
        if pos == to {
            let mut path = vec![to];
            while let Some(&prev) = came_from.get(path.last().unwrap()) {
                if prev == from {
                    break;
                }
                path.push(prev);
            }
            path.reverse();
            return Some(path);
        }
        // Stale queue entry (a better route to `pos` was already expanded).
        if best_cost.get(&pos).is_some_and(|c| *c < cost) {
            continue;
        }
        for next in neighbors(map, pos, &blocked) {
            let next_cost = cost + 1;
            if next_cost < *best_cost.get(&next).unwrap_or(&u32::MAX) {
                best_cost.insert(next, next_cost);
                came_from.insert(next, pos);
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

fn neighbors(map: &ShipMap, p: TilePos, blocked: &impl Fn(TilePos) -> bool) -> Vec<TilePos> {
    let mut out = Vec::with_capacity(4);
    for n in [
        TilePos::new(p.x + 1, p.y),
        TilePos::new(p.x - 1, p.y),
        TilePos::new(p.x, p.y + 1),
        TilePos::new(p.x, p.y - 1),
    ] {
        if map.is_walkable(n) && !blocked(n) {
            out.push(n);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> ShipMap {
        ShipMap::from_layout(&["#####", "#...#", "#.#.#", "#...#", "#####"]).0
    }

    #[test]
    fn straight_line() {
        let m = map();
        let path = find_path(&m, TilePos::new(1, 1), TilePos::new(3, 1), |_| false).unwrap();
        assert_eq!(path, vec![TilePos::new(2, 1), TilePos::new(3, 1)]);
    }

    #[test]
    fn detours_around_wall() {
        let m = map();
        // (1,2) and (3,2) are separated by the wall at (2,2): must go around.
        let path = find_path(&m, TilePos::new(1, 2), TilePos::new(3, 2), |_| false).unwrap();
        assert_eq!(path.len(), 4); // up, right, right, down
        assert_eq!(*path.last().unwrap(), TilePos::new(3, 2));
    }

    #[test]
    fn unreachable_returns_none() {
        // Two pockets touching only diagonally: no 4-directional route.
        let m = ShipMap::from_layout(&["#####", "#..##", "##..#", "#####"]).0;
        assert!(find_path(&m, TilePos::new(1, 1), TilePos::new(4, 2), |_| false).is_none());
    }

    #[test]
    fn goal_blocked_by_dynamic_obstacle() {
        let m = map();
        assert!(find_path(&m, TilePos::new(1, 1), TilePos::new(3, 1), |p| p
            == TilePos::new(3, 1))
        .is_none());
    }

    #[test]
    fn zero_length_path_when_already_there() {
        let m = map();
        assert!(
            find_path(&m, TilePos::new(2, 1), TilePos::new(2, 1), |_| false)
                .unwrap()
                .is_empty()
        );
    }
}
