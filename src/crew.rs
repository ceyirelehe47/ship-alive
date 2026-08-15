//! Crew members: state, task bookkeeping and movement data.

use bevy::prelude::*;

#[derive(Component)]
pub struct Crew {
    pub name: String,
    pub tint: Color,
    /// Tiles per (virtual) second at 1x speed.
    pub speed: f32,
    /// Game time (seconds) after which this crew rescans for work.
    pub next_scan: f64,
    /// Completed haul jobs — surfaced in the UI as an efficiency signal.
    pub delivered: u32,
}

impl Crew {
    pub fn new(name: &str, tint: Color) -> Self {
        Self {
            name: name.to_string(),
            tint,
            speed: 3.0,
            next_scan: 0.0,
            delivered: 0,
        }
    }
}

/// Why an idle crew member currently has no work. Shown in the UI.
#[derive(Clone, PartialEq, Debug)]
pub enum IdleCause {
    /// Fresh idle (will scan for work shortly).
    Looking,
    /// No items marked for hauling.
    NoMarkedItems,
    /// Marked items exist but every one is claimed by someone else.
    AllClaimed,
    /// Marked items exist but storage has no free capacity.
    NoStorageSpace,
    /// Marked items exist but none is reachable.
    AllUnreachable,
    /// Job was cancelled (player unmarked the item, or target vanished).
    JobCanceled { detail: String },
    /// Had to drop the carried item (e.g. storage filled up mid-delivery).
    JobFailed { detail: String },
}

impl IdleCause {
    pub fn label(&self) -> String {
        match self {
            IdleCause::Looking => "Looking for work…".into(),
            IdleCause::NoMarkedItems => "Idle — nothing marked for hauling".into(),
            IdleCause::AllClaimed => "Idle — all marked items already claimed".into(),
            IdleCause::NoStorageSpace => "Idle — no free storage space".into(),
            IdleCause::AllUnreachable => "Idle — marked items unreachable".into(),
            IdleCause::JobCanceled { detail } => format!("Job canceled — {detail}"),
            IdleCause::JobFailed { detail } => format!("Job failed — {detail}"),
        }
    }
}

/// One haul job: take `item` to a storage rack.
#[derive(Clone, Debug)]
pub struct HaulJob {
    pub item: Entity,
    pub phase: HaulPhase,
    /// Rack the crew intends to deliver to; re-evaluated on arrival.
    pub target_rack: Option<Entity>,
    /// Consecutive failed path attempts (used to give up and drop).
    pub repaths: u32,
    /// Seconds spent in a delay phase (pickup/store animation beats).
    pub timer: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HaulPhase {
    ToItem,
    PickingUp,
    ToStorage,
    Storing,
}

impl HaulPhase {
    pub fn label(&self) -> &'static str {
        match self {
            HaulPhase::ToItem => "Going to item",
            HaulPhase::PickingUp => "Picking up",
            HaulPhase::ToStorage => "Carrying to storage",
            HaulPhase::Storing => "Storing",
        }
    }
}

/// Current occupation of a crew member.
#[derive(Component, Clone, Debug)]
pub enum CrewTask {
    Idle(IdleCause),
    Haul(HaulJob),
}

impl Default for CrewTask {
    fn default() -> Self {
        CrewTask::Idle(IdleCause::Looking)
    }
}

/// Walk-along-path state. An empty path means "not moving".
#[derive(Component, Default)]
pub struct Movement {
    pub path: Vec<crate::map::TilePos>,
    /// Progress (0..1) toward `path[0]`.
    pub progress: f32,
    /// Seconds blocked by another crew member standing on the next tile.
    pub blocked_for: f32,
    /// Set when giving up on avoidance — walk through the blocker.
    pub passing_through: bool,
}
