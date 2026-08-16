//! Crew movement: walk along the A* path tile by tile.
//!
//! Soft avoidance, layered (simplest first):
//! 1. **Head-on priority** — when two crew face each other in a one-wide
//!    corridor, the one with the lower entity id passes through immediately.
//!    Deterministic, no flicker, resolves the standoff in one frame.
//! 2. **Sidestep yield** — when blocked longer than SIDESTEP_AFTER by a
//!    non-head-on blocker and a free orthogonal neighbor exists, the crew
//!    steps aside to let the blocker through, then resumes its own path.
//! 3. **Re-path** around blockers (crews treated as walls).
//! 4. **Pass-through** after PASS_THROUGH_AFTER as the hard fallback, so a
//!    congestion chain can never deadlock permanently.

use crate::crew::{Crew, Movement};
use crate::map::{ShipMap, TilePos};
use bevy::prelude::*;

/// Seconds of accumulated blocking before re-pathing around the blocker.
const REPATH_AFTER: f32 = 0.6;
/// Seconds of blocking after which the crew walks through it.
const PASS_THROUGH_AFTER: f32 = 1.5;
/// Seconds of blocking before trying to sidestep out of the way.
const SIDESTEP_AFTER: f32 = 0.35;
/// Cooldown between sidesteps so yielding cannot oscillate.
const SIDESTEP_COOLDOWN: f32 = 1.2;
/// Hard ceiling without any tile advance before forcing pass-through — the
/// monotone backstop above every other avoidance mechanism.
const STUCK_ABORT: f32 = 2.5;

pub struct MovementPlugin;

impl Plugin for MovementPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, movement_system.in_set(crate::Set::Move));
    }
}

pub fn movement_system(
    map: Res<ShipMap>,
    time: Res<Time<Virtual>>,
    mut crews: Query<(Entity, &Crew, &mut TilePos, &mut Movement)>,
) {
    let dt = time.delta().as_secs_f32();

    // Occupancy snapshot: tiles a crew member physically stands on right now.
    // "Entering" tiles are deliberately NOT reserved: two crew may pick the
    // same next tile; the one that arrives first becomes the standing blocker
    // for the other. This avoids circular waits between mutual `entering`
    // claims (a livelock observed at one-wide doors during scenario tests).
    let occupied: Vec<(TilePos, Entity)> = crews.iter().map(|(e, _, pos, _)| (*pos, e)).collect();

    // Where each crew is heading (path[0]), for head-on detection.
    let headings: Vec<(Entity, TilePos, TilePos)> = crews
        .iter()
        .filter(|(_, _, _, m)| !m.path.is_empty())
        .map(|(e, _, pos, m)| (e, *pos, m.path[0]))
        .collect();

    let is_free = |p: TilePos| map.is_walkable(p) && !occupied.iter().any(|&(o, _)| o == p);

    for (entity, crew, mut pos, mut mov) in crews.iter_mut() {
        let pos_before = *pos;
        if mov.path.is_empty() {
            mov.progress = 0.0;
            mov.blocked_for = 0.0;
            mov.passing_through = false;
            mov.blocked_tile = None;
            mov.blocked_on_tile = 0.0;
            mov.sidestep_ready = 0.0;
            mov.yield_for = None;
            mov.stuck_for = 0.0;
            continue;
        }
        let next = mov.path[0];
        mov.sidestep_ready = (mov.sidestep_ready + dt).min(SIDESTEP_COOLDOWN);
        let blocked_by = occupied
            .iter()
            .find(|(stand, other)| *other != entity && *stand == next)
            .map(|(_, other)| *other);

        // Track per-target-tile blockage without decay: a mutual head-on
        // standoff must escalate to pass-through within PASS_THROUGH_AFTER no
        // matter how the decaying `blocked_for` oscillates on frames where
        // the blocker's tile momentarily reads as free.
        if mov.blocked_tile != Some(next) {
            mov.blocked_tile = Some(next);
            mov.blocked_on_tile = 0.0;
        }

        let blocked = blocked_by.is_some() && !mov.passing_through;
        if blocked {
            mov.blocked_for += dt;
            mov.blocked_on_tile += dt;
            let blocker = blocked_by.unwrap();

            // Already yielded once for this exact obstacle? Go through — a
            // second sidestep would just reset the clocks and ping-pong.
            if mov.yield_for == Some(next) {
                mov.passing_through = true;
                continue;
            }

            // (1) Head-on priority: we want the blocker's tile and it wants
            // ours. The lower entity id walks through immediately; the other
            // stands and waits. Deterministic and instant.
            let head_on = headings.iter().any(|(other, other_pos, other_next)| {
                *other == blocker && *other_next == *pos && *other_pos == next
            });
            if head_on && entity < blocker {
                mov.passing_through = true;
                continue;
            }

            if mov.blocked_on_tile >= PASS_THROUGH_AFTER || mov.blocked_for >= PASS_THROUGH_AFTER {
                mov.passing_through = true;
            } else if mov.blocked_for >= SIDESTEP_AFTER
                && mov.sidestep_ready >= SIDESTEP_COOLDOWN
                && !head_on
                && mov.yield_for != Some(next)
            {
                // (2) Sidestep yield: step onto a free orthogonal neighbor
                // (never backward into our own trail unless it is the only
                // option), splicing it in front of the original path.
                // Sidestep candidates are the two "corners" beside the
                // blockage: tiles 4-adjacent to BOTH us and the blocked
                // tile. Movement is 4-directional, so after stepping aside
                // the return step onto the original path stays legal — a
                // plain free neighbor could be diagonal to `next`.
                let corners = [
                    TilePos::new(next.x + 1, next.y),
                    TilePos::new(next.x - 1, next.y),
                    TilePos::new(next.x, next.y + 1),
                    TilePos::new(next.x, next.y - 1),
                ];
                let options: Vec<TilePos> = corners
                    .into_iter()
                    .filter(|p| *p != *pos && is_free(*p))
                    .collect();
                if let Some(step) = options.first().copied() {
                    mov.path.insert(0, step);
                    mov.blocked_for = 0.0;
                    mov.blocked_on_tile = 0.0;
                    mov.blocked_tile = None;
                    mov.sidestep_ready = 0.0;
                    mov.yield_for = Some(next);
                }
            } else if mov.blocked_for >= REPATH_AFTER {
                // (3) Try a route that treats other crews' tiles as walls.
                let goal = *mov.path.last().unwrap();
                let blockers: Vec<TilePos> = occupied
                    .iter()
                    .filter(|&&(_, other)| other != entity)
                    .map(|&(stand, _)| stand)
                    .collect();
                if let Some(alt) =
                    crate::path::find_path(&map, *pos, goal, |p| blockers.contains(&p))
                {
                    // Only accept a plausible detour, not a huge loop: compare
                    // real path costs (diagonals are not "1 node" anymore).
                    let alt_cost = crate::path::path_cost(Some(*pos), &alt);
                    let cur_cost = crate::path::path_cost(Some(*pos), &mov.path);
                    if alt_cost < cur_cost + 4 * crate::path::COST_CARDINAL {
                        mov.path = alt;
                        mov.blocked_for = 0.0;
                    }
                }
            }
            continue;
        }

        // Not blocked right now: decay blocking pressure instead of resetting
        // it, so rapidly oscillating congestion still escalates to
        // pass-through instead of livelocking at a crawl.
        mov.blocked_for = (mov.blocked_for - dt * 2.0).max(0.0);
        if mov.blocked_for == 0.0 {
            mov.passing_through = false;
        }
        // `progress` is a DISTANCE budget toward `path[0]` in tile units:
        // a diagonal step costs √2 of budget, a cardinal step 1, so the
        // world-space speed stays `crew.speed` per second in every
        // direction (no 41% diagonal speed boost). Leftover budget carries
        // across steps with per-step conversion.
        mov.progress += dt * crew.speed;
        while !mov.path.is_empty() {
            let need = crate::path::step_length(*pos, mov.path[0]);
            if mov.progress < need {
                break;
            }
            mov.progress -= need;
            let entered = mov.path.remove(0);
            *pos = entered;
            // Real progress: the obstacle we yielded for is behind us.
            if mov.yield_for == Some(entered) {
                mov.yield_for = None;
            }
        }

        // Monotone watchdog: sidesteps and re-paths swap `path[0]` around
        // and can reset every per-tile clock. Only an actual tile advance
        // counts as progress; without one for STUCK_ABORT seconds, force
        // pass-through until a step happens.
        if *pos != pos_before {
            mov.stuck_for = 0.0;
        } else if !mov.path.is_empty() {
            mov.stuck_for += dt;
            if mov.stuck_for >= STUCK_ABORT {
                mov.passing_through = true;
            }
        }
    }
}
