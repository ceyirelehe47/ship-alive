//! Ship Alive — Playable Slice 0.
//!
//! A tiny colony-sim slice: 4 crew live inside a fixed-layout starter ship,
//! pick up haul jobs, walk to items, carry them and store them into racks.
//!
//! Module layout keeps gameplay simulation (map, pathfinding, jobs, movement)
//! independent from presentation (render, ui), so the simulation can be unit
//! tested with a bare `bevy_ecs` `World`.

use bevy::prelude::*;

pub mod airtight;
pub mod autotest;
pub mod building;
pub mod coolant;
pub mod crew;
pub mod input;
pub mod items;
pub mod jobs;
pub mod log;
pub mod map;
pub mod movement;
pub mod path;
pub mod power;
pub mod production;
pub mod render;
pub mod setup;
pub mod simtime;
pub mod stats;
pub mod storage;
pub mod thermal;
pub mod time_ctrl;
pub mod ui;
pub mod ui_overlay;

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

/// Exclusive full-map overlay view (power / thermal / coolant /
/// compartments). Mutually exclusive by construction — one resource, one
/// active mode.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OverlayMode {
    #[default]
    Off,
    Power,
    Thermal,
    Coolant,
    Compartments,
}

impl OverlayMode {
    pub fn label(self) -> &'static str {
        match self {
            OverlayMode::Off => "Off",
            OverlayMode::Power => "Power",
            OverlayMode::Thermal => "Thermal",
            OverlayMode::Coolant => "Coolant",
            OverlayMode::Compartments => "Compartments",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            OverlayMode::Off => OverlayMode::Power,
            OverlayMode::Power => OverlayMode::Thermal,
            OverlayMode::Thermal => OverlayMode::Coolant,
            OverlayMode::Coolant => OverlayMode::Compartments,
            OverlayMode::Compartments => OverlayMode::Off,
        }
    }
}
