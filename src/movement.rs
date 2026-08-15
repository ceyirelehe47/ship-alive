//! Crew movement: walk along the A* path tile by tile.
//!
//! Soft avoidance: a crew member will not enter a tile another crew member
//! stands on (or is stepping into). If blocked, they wait briefly, try to
//! re-path around the blocker, and as a last resort walk through it. This
//! keeps one-wide corridors visibly congested without ever dead-locking.

use crate::crew::{Crew, Movement};
use crate::map::{ShipMap, TilePos};
use bevy::prelude::*;

/// Seconds of accumulated blocking before re-pathing around the blocker.
const REPATH_AFTER: f32 = 0.6;
/// Seconds of accumulated blocking after which the crew walks through it.
const PASS_THROUGH_AFTER: f32 = 1.5;

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

    for (entity, crew, mut pos, mut mov) in crews.iter_mut() {
        if mov.path.is_empty() {
            mov.progress = 0.0;
            mov.blocked_for = 0.0;
            mov.passing_through = false;
            continue;
        }
        let next = mov.path[0];
        let blocked =
            !mov.passing_through && occupied.iter().any(|&(stand, other)| other != entity && stand == next);

        if blocked {
            mov.blocked_for += dt;
            if mov.blocked_for >= PASS_THROUGH_AFTER {
                mov.passing_through = true;
            } else if mov.blocked_for >= REPATH_AFTER {
                // Try a route that treats other crews' tiles as walls.
                let goal = *mov.path.last().unwrap();
                let blockers: Vec<TilePos> = occupied
                    .iter()
                    .filter(|&&(_, other)| other != entity)
                    .map(|&(stand, _)| stand)
                    .collect();
                if let Some(alt) = crate::path::find_path(&map, *pos, goal, |p| blockers.contains(&p)) {
                    // Only accept if it is a plausible detour, not a huge loop.
                    if alt.len() < mov.path.len() + 4 {
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
        mov.progress += dt * crew.speed;
        while mov.progress >= 1.0 && !mov.path.is_empty() {
            *pos = mov.path.remove(0);
            mov.progress -= 1.0;
        }
    }
}
