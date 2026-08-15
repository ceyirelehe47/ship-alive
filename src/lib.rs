//! Ship Alive — Playable Slice 0.
//!
//! A tiny colony-sim slice: 4 crew live inside a fixed-layout starter ship,
//! pick up haul jobs, walk to items, carry them and store them into racks.
//!
//! Module layout keeps gameplay simulation (map, pathfinding, jobs, movement)
//! independent from presentation (render, ui), so the simulation can be unit
//! tested with a bare `bevy_ecs` `World`.

use bevy::prelude::*;

pub mod autotest;
pub mod crew;
pub mod input;
pub mod items;
pub mod jobs;
pub mod log;
pub mod map;
pub mod movement;
pub mod path;
pub mod render;
pub mod setup;
pub mod storage;
pub mod time_ctrl;
pub mod ui;

/// World-space size of one map tile.
pub const TILE: f32 = 32.0;

/// Multipliers for the speed control (see `time_ctrl`).
pub const SPEED_STEPS: [f32; 4] = [0.0, 1.0, 2.0, 4.0];

/// Frame-wide system ordering. Input (player intent) is consumed first, then
/// jobs advance, then movement, then visuals sync.
#[derive(SystemSet, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Set {
    Input,
    Jobs,
    Move,
    Sync,
}
