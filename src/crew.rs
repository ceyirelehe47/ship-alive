//! Crew members: state, task bookkeeping and movement data.
//!
//! Slice 1 generalizes work into three categories — haul, build
//! (construct/deconstruct) and operate (production) — each with a
//! player-configurable priority that decides which jobs a crew member takes
//! when several are available.

use crate::items::ItemKind;
use bevy::prelude::*;

#[derive(Component)]
pub struct Crew {
    pub name: String,
    pub tint: Color,
    /// Tiles per (virtual) second at 1x speed.
    pub speed: f32,
    /// Game time (seconds) after which this crew rescans for work.
    pub next_scan: f64,
    /// Completed haul deliveries — surfaced in the UI as an efficiency signal.
    pub delivered: u32,
    /// Completed build / deconstruct / operate jobs.
    pub built: u32,
    pub operated: u32,
    /// Per-work-type priority (player configurable).
    pub priorities: WorkPriorities,
}

impl Crew {
    pub fn new(name: &str, tint: Color) -> Self {
        Self {
            name: name.to_string(),
            tint,
            speed: 3.0,
            next_scan: 0.0,
            delivered: 0,
            built: 0,
            operated: 0,
            priorities: WorkPriorities::default(),
        }
    }
}

/// The three work categories a crew member can be assigned to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WorkKind {
    Haul,
    Build,
    Operate,
}

impl WorkKind {
    pub const ALL: [WorkKind; 3] = [WorkKind::Haul, WorkKind::Build, WorkKind::Operate];

    pub fn label(&self) -> &'static str {
        match self {
            WorkKind::Haul => "Haul",
            WorkKind::Build => "Build",
            WorkKind::Operate => "Operate",
        }
    }
}

/// Player-set priority for one work type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Priority {
    Disabled,
    Low,
    Normal,
    High,
}

impl Priority {
    pub const ALL: [Priority; 4] = [
        Priority::Disabled,
        Priority::Low,
        Priority::Normal,
        Priority::High,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Priority::Disabled => "Off",
            Priority::Low => "Low",
            Priority::Normal => "Normal",
            Priority::High => "High",
        }
    }

    /// Dominance weight used by the job scan: higher tiers always beat lower
    /// tiers; distance only breaks ties inside a tier.
    pub fn weight(self) -> i32 {
        match self {
            Priority::Disabled => 0,
            Priority::Low => 200,
            Priority::Normal => 500,
            Priority::High => 1000,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WorkPriorities {
    pub haul: Priority,
    pub build: Priority,
    pub operate: Priority,
}

impl Default for WorkPriorities {
    fn default() -> Self {
        Self {
            haul: Priority::Normal,
            build: Priority::Normal,
            operate: Priority::Normal,
        }
    }
}

impl WorkPriorities {
    pub fn get(&self, kind: WorkKind) -> Priority {
        match kind {
            WorkKind::Haul => self.haul,
            WorkKind::Build => self.build,
            WorkKind::Operate => self.operate,
        }
    }

    pub fn set(&mut self, kind: WorkKind, level: Priority) {
        match kind {
            WorkKind::Haul => self.haul = level,
            WorkKind::Build => self.build = level,
            WorkKind::Operate => self.operate = level,
        }
    }
}

/// Why an idle crew member currently has no work. Shown in the UI.
#[derive(Clone, PartialEq, Debug)]
pub enum IdleCause {
    /// Fresh idle (will scan for work shortly).
    Looking,
    /// Nothing to do anywhere on the ship.
    NothingToDo,
    /// Haulable items exist but every one is claimed by someone else.
    AllClaimed,
    /// Haulable items exist but storage has no free capacity for them.
    NoStorageSpace,
    /// Items exist but none is reachable.
    AllUnreachable,
    /// Every enabled work type is disabled by the player.
    AllWorkDisabled,
    /// Job was cancelled (player unmarked the item, or target vanished).
    JobCanceled { detail: String },
    /// Had to drop the carried item (e.g. storage filled up mid-delivery).
    JobFailed { detail: String },
}

impl IdleCause {
    pub fn label(&self) -> String {
        match self {
            IdleCause::Looking => "Looking for work…".into(),
            IdleCause::NothingToDo => "Idle — nothing to do".into(),
            IdleCause::AllClaimed => "Idle — all marked items already claimed".into(),
            IdleCause::NoStorageSpace => "Idle — no free storage space".into(),
            IdleCause::AllUnreachable => "Idle — marked items unreachable".into(),
            IdleCause::AllWorkDisabled => "Idle — all work types disabled".into(),
            IdleCause::JobCanceled { detail } => format!("Job canceled — {detail}"),
            IdleCause::JobFailed { detail } => format!("Job failed — {detail}"),
        }
    }
}

/// One haul job: take `item` somewhere. The destination is generalized in
/// Slice 1: storage (player haul), a construction blueprint, or a fabricator
/// input buffer (auto-logistics).
#[derive(Clone, Debug)]
pub struct HaulJob {
    pub item: Entity,
    pub phase: HaulPhase,
    pub dest: HaulDest,
    /// Storage destination only: rack chosen at pickup, re-evaluated on arrival.
    pub target_rack: Option<Entity>,
    /// Seconds spent in a delay phase (pickup/store animation beats).
    pub timer: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HaulDest {
    /// Nearest rack that accepts the item kind (Slice 0 behavior).
    Storage,
    /// Deliver onto a construction blueprint.
    Blueprint(Entity),
    /// Deliver into a fabricator's input buffer.
    Machine(Entity),
}

impl HaulDest {
    pub fn label(&self) -> String {
        match self {
            HaulDest::Storage => "storage".into(),
            HaulDest::Blueprint(_) => "blueprint".into(),
            HaulDest::Machine(_) => "fabricator".into(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HaulPhase {
    ToItem,
    PickingUp,
    ToDest,
    Delivering,
}

impl HaulPhase {
    pub fn label(&self) -> &'static str {
        match self {
            HaulPhase::ToItem => "Going to item",
            HaulPhase::PickingUp => "Picking up",
            HaulPhase::ToDest => "Carrying to destination",
            HaulPhase::Delivering => "Delivering",
        }
    }
}

/// Walk-to-target then work-for-a-duration job used by construction,
/// deconstruction and machine operation.
#[derive(Clone, Debug)]
pub struct WorkJob {
    pub target: Entity,
    pub phase: WorkPhase,
    pub timer: f32,
    /// Total work seconds (for progress display).
    pub total: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WorkPhase {
    Going,
    Working,
}

/// Current occupation of a crew member.
#[derive(Component, Clone, Debug)]
pub enum CrewTask {
    Idle(IdleCause),
    Haul(HaulJob),
    Build(WorkJob),
    Deconstruct(WorkJob),
    Operate(WorkJob),
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
    /// The tile we are currently trying to enter; `blocked_on_tile` is the
    /// *undecayed* time the same tile has blocked us. Head-on standoffs in
    /// one-wide corridors must escalate to pass-through even when the
    /// regular `blocked_for` decays on intermittently-free frames.
    pub blocked_tile: Option<crate::map::TilePos>,
    pub blocked_on_tile: f32,
}

/// Reason of one unit of material demand (used by the logistics scan).
#[derive(Clone, Copy, Debug)]
pub struct MaterialNeed {
    /// Blueprint or fabricator entity that wants the material.
    pub consumer: Entity,
    pub kind: ItemKind,
}
