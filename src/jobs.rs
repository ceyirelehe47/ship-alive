//! The work system: player actions, job claiming (with priorities), phase
//! transitions and failure recovery for all three work categories — haul,
//! build (construct/deconstruct) and operate (production).
//!
//! There is no separate "job board" data structure. Jobs are derived state:
//! `MarkedForHaul` on an item is the player's intent, a blueprint's missing
//! materials are an auto-logistics demand, `ReservedBy` is the claim, and the
//! claiming crew's `CrewTask` holds the execution state. This keeps a single
//! source of truth per question and makes stale reservations impossible as
//! long as every code path that ends a job also releases the claim.

use crate::building::{self, Blueprint, Building, BuildingKind, Footprint, MarkedForDeconstruct};
use crate::crew::{
    Crew, CrewTask, HaulDest, HaulJob, HaulPhase, IdleCause, Movement, Priority, WorkJob, WorkKind,
    WorkPhase,
};
use crate::items::{CarriedBy, Item, ItemKind, MarkedForHaul, NoPathUntil, ReservedBy};
use crate::log::{EventLog, LogKind};
use crate::map::{find_drop_tile, ShipMap, TilePos};
use crate::power::{CableGrid, PowerRole, PowerStatus};
use crate::production::Fabricator;
use crate::simtime::{SimClock, BASE_SIM_RATE};
use crate::storage::StorageCell;

// ---- gameplay durations in SIM seconds (1 real s at 1× = 60 sim s) ----
/// Haul pickup beat (was 0.3 real s).
const PICKUP_SECS: f32 = 0.3 * BASE_SIM_RATE as f32;
/// Deposit/store beat (was 0.25 real s).
const DELIVER_SECS: f32 = 0.25 * BASE_SIM_RATE as f32;
/// Rescan cooldown after a canceled job (was 0.2 real s).
const RESCAN_CANCELED: f32 = 0.2 * BASE_SIM_RATE as f32;
/// Rescan cooldown after a failed job (was 0.3 real s).
const RESCAN_FAILED: f32 = 0.3 * BASE_SIM_RATE as f32;
/// Rescan cooldown when unreachable (was 0.5 real s).
const RESCAN_UNREACHABLE: f32 = 0.5 * BASE_SIM_RATE as f32;
/// Idle scan cadence (was 0.6 real s).
const SCAN_IDLE: f32 = 0.6 * BASE_SIM_RATE as f32;
/// Slower re-scan when repeatedly nothing to do (was 1.0 real s).
const SCAN_IDLE_SLOW: f32 = 1.0 * BASE_SIM_RATE as f32;
use bevy::prelude::*;
use std::collections::{HashMap, HashSet};

/// Player-facing actions, produced by keyboard shortcuts and UI buttons alike.
#[derive(Event, Clone, Copy, Debug)]
pub enum Action {
    /// Toggle the haul mark of the selected item.
    ToggleMark { item: Entity },
    /// Mark every ground item for hauling.
    MarkAll,
    /// Box-select: mark every (uncarried) ground item whose world position is
    /// inside the rectangle spanned by the two world-space corners.
    MarkArea { from: Vec2, to: Vec2 },
    /// Unmark everything and cancel all running haul jobs (drop carried items).
    CancelAll,
    /// Debug: remove the selected item entity from the world.
    DeleteItem { item: Entity },
    /// Debug: spawn one item of `kind` on a random free tile of the cargo hold.
    SpawnItem { kind: ItemKind },
    /// UI-only: show/hide the developer toolbar (consumed by the UI plugin).
    ToggleDebug,
    /// Set simulation speed by index into [`crate::simtime::SPEED_SCALES`].
    SetSpeed { index: usize },
    /// Toggle between Pause and the last non-paused speed (Space).
    TogglePause,
    // ---- Slice 1: construction -----------------------------------------
    /// Place a construction blueprint (origin = top-left footprint tile).
    PlaceBlueprint { kind: BuildingKind, pos: TilePos },
    /// Cancel a blueprint; refunds any materials already on site.
    CancelBlueprint { blueprint: Entity },
    /// Mark a building for deconstruction.
    MarkDeconstruct { building: Entity },
    /// Undo a deconstruction mark.
    UnmarkDeconstruct { building: Entity },
    // ---- Slice 1: storage ----------------------------------------------
    /// Allow or deny one item kind on a rack.
    SetRackFilter {
        rack: Entity,
        kind: ItemKind,
        allowed: bool,
    },
    // ---- Slice 1: production -------------------------------------------
    /// Add `batches` to a fabricator's order.
    FabAddOrder { fab: Entity, batches: u32 },
    /// Toggle endless repeat on a fabricator's order.
    FabRepeat { fab: Entity },
    /// Clear a fabricator's order (buffered input is kept).
    FabClearOrder { fab: Entity },
    // ---- Slice 1: work priorities --------------------------------------
    SetPriority {
        crew: Entity,
        work: WorkKind,
        level: Priority,
    },
    /// UI-only: select a build tool (consumed by the input plugin).
    SetTool { tool: Option<crate::input::Tool> },
    // ---- Slice 2: power --------------------------------------------------
    /// UI-only: cycle the map overlay view (off → power → thermal → coolant).
    CycleOverlay,
    /// Turn a reactor on or off.
    SetGeneratorOn { gen: Entity, on: bool },
    /// Mark an underfloor cable tile for deconstruction (spawns the
    /// transient tile entity the work system tears down).
    MarkCableDeconstruct { pos: TilePos },
    /// Mark an underfloor coolant pipe tile for deconstruction (spawns the
    /// transient tile entity the work system tears down; water is preserved
    /// into the network where possible).
    MarkPipeDeconstruct { pos: TilePos },
}

pub struct JobsPlugin;

impl Plugin for JobsPlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<Action>();
        app.init_resource::<crate::stats::Stats>();
        // Player actions are frame-based (events must fire exactly once per
        // frame); everything downstream advances per fixed sim step.
        app.add_systems(Update, actions_system.in_set(crate::Set::Input));
        app.add_systems(
            FixedUpdate,
            (
                crate::power::power_network_system,
                crate::thermal::thermal_air_system,
                crate::coolant::coolant_system,
                crate::thermal::thermal_state_system,
                crew_task_system,
                crew_scan_system,
            )
                .chain()
                .in_set(crate::Set::Jobs),
        );
    }
}

// =====================================================================================
// Player actions
// =====================================================================================

/// Handle player actions that mutate job/item/building state. Runs before job
/// updates so cancelled jobs settle within the same frame.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
pub fn actions_system(
    mut events: EventReader<Action>,
    mut commands: Commands,
    map: Res<ShipMap>,
    mut log: ResMut<EventLog>,
    clock: Res<SimClock>,
    mut crews: Query<(Entity, &mut Crew, &mut CrewTask, &TilePos, &mut Movement), Without<Item>>,
    items: Query<(Entity, &TilePos, Option<&MarkedForHaul>), With<Item>>,
    rack_tiles: Query<&TilePos, (With<StorageCell>, Without<Crew>)>,
    blueprints: Query<(Entity, &Footprint, &Blueprint)>,
    buildings: Query<(Entity, &Footprint), (With<Building>, Without<Blueprint>)>,
    mut racks: Query<(Entity, &TilePos, &mut StorageCell), Without<Crew>>,
    mut fabs: Query<(Entity, &mut Fabricator), Without<Crew>>,
    mut gens: Query<(Entity, &mut PowerRole)>,
    cables: Res<CableGrid>,
    pipes: Res<crate::coolant::PipeGrid>,
    cable_tiles: Query<(Entity, &TilePos), (With<Building>, With<MarkedForDeconstruct>)>,
) {
    let now = clock.now();
    let racks_list: Vec<(TilePos, Entity)> = rack_tiles
        .iter()
        .map(|p| (*p, Entity::PLACEHOLDER))
        .collect();

    for action in events.read() {
        match *action {
            Action::ToggleMark { item } => {
                if let Ok((_, _, marked)) = items.get(item) {
                    if marked.is_some() {
                        end_hauls_for_item(
                            &mut crews,
                            &mut commands,
                            &map,
                            &racks_list,
                            item,
                            &mut log,
                            now,
                            "player unmarked the item".into(),
                        );
                        commands.entity(item).remove::<MarkedForHaul>();
                        log.push(now, LogKind::Info, "Item unmarked");
                    } else {
                        commands.entity(item).insert(MarkedForHaul);
                        log.push(now, LogKind::Info, "Item marked for hauling");
                    }
                }
            }
            Action::MarkAll => {
                let mut n = 0;
                for (e, _, marked) in items.iter() {
                    if marked.is_none() {
                        commands.entity(e).insert(MarkedForHaul);
                        n += 1;
                    }
                }
                log.push(now, LogKind::Info, format!("Marked {n} items for hauling"));
            }
            Action::MarkArea { from, to } => {
                let (min_x, max_x) = (from.x.min(to.x), from.x.max(to.x));
                let (min_y, max_y) = (from.y.min(to.y), from.y.max(to.y));
                let mut n = 0;
                for (e, pos, marked) in items.iter() {
                    if marked.is_some() {
                        continue;
                    }
                    let p = map.world_pos(*pos);
                    if p.x >= min_x && p.x <= max_x && p.y >= min_y && p.y <= max_y {
                        commands.entity(e).insert(MarkedForHaul);
                        n += 1;
                    }
                }
                if n > 0 {
                    log.push(
                        now,
                        LogKind::Info,
                        format!("Marked {n} items for hauling (box select)"),
                    );
                }
            }
            Action::CancelAll => {
                for (e, _, marked) in items.iter() {
                    if marked.is_some() {
                        commands.entity(e).remove::<MarkedForHaul>();
                    }
                }
                let mut canceled = 0;
                for (_, crew, mut task, pos, mut mov) in crews.iter_mut() {
                    if matches!(*task, CrewTask::Haul(_)) {
                        let item = match &*task {
                            CrewTask::Haul(job) => job.item,
                            _ => unreachable!(),
                        };
                        let name = crew.name.clone();
                        end_haul(
                            &mut task,
                            &mut mov,
                            &mut commands,
                            &map,
                            &racks_list,
                            *pos,
                            &name,
                            item,
                            &mut log,
                            now,
                            IdleCause::JobCanceled {
                                detail: "player canceled all jobs".into(),
                            },
                        );
                        canceled += 1;
                    }
                }
                log.push(now, LogKind::Info, format!("Canceled {canceled} haul jobs"));
            }
            Action::DeleteItem { item } => {
                if items.get(item).is_ok() {
                    end_hauls_for_item(
                        &mut crews,
                        &mut commands,
                        &map,
                        &racks_list,
                        item,
                        &mut log,
                        now,
                        "target deleted".into(),
                    );
                    commands.entity(item).despawn();
                    log.push(now, LogKind::Fail, "Item deleted");
                }
            }
            Action::SpawnItem { kind } => {
                if let Some(pos) = random_cargo_tile(&map, &items) {
                    let e = crate::items::spawn_item(&mut commands, pos, kind);
                    commands.entity(e).insert(MarkedForHaul);
                    log.push(
                        now,
                        LogKind::Info,
                        format!("Spawned {} at ({},{})", kind.label(), pos.x, pos.y),
                    );
                } else {
                    log.push(now, LogKind::Fail, "No free cargo tile to spawn on");
                }
            }
            Action::PlaceBlueprint { kind, pos } => {
                let ground: Vec<TilePos> = items.iter().map(|(_, p, _)| *p).collect();
                let mut feet: Vec<(Footprint, bool)> =
                    blueprints.iter().map(|(_, f, _)| (*f, true)).collect();
                feet.extend(buildings.iter().map(|(_, f)| (*f, false)));
                match building::can_place(
                    &map,
                    kind,
                    pos,
                    &ground,
                    &feet,
                    |p| cables.has(p),
                    |p| pipes.has(p),
                ) {
                    Ok(()) => {
                        building::spawn_blueprint(&mut commands, kind, pos);
                        log.push(
                            now,
                            LogKind::Info,
                            format!("{} blueprint placed at ({},{})", kind.label(), pos.x, pos.y),
                        );
                    }
                    Err(e) => {
                        log.push(
                            now,
                            LogKind::Fail,
                            format!("Cannot build {}: {}", kind.label(), e.label()),
                        );
                    }
                }
            }
            Action::CancelBlueprint { blueprint } => {
                if let Ok((_, foot, bp)) = blueprints.get(blueprint) {
                    let (foot, delivered, kind) = (*foot, bp.delivered, bp.kind);
                    cancel_blueprint(
                        &mut commands,
                        &mut crews,
                        &map,
                        &racks_list,
                        blueprint,
                        delivered,
                        kind,
                        foot,
                        &mut log,
                        now,
                    );
                }
            }
            Action::MarkDeconstruct { building } => {
                if buildings.iter().any(|(e, _)| e == building) {
                    commands.entity(building).insert(MarkedForDeconstruct);
                    log.push(now, LogKind::Info, "Building marked for deconstruction");
                } else {
                    log.push(now, LogKind::Fail, "That cannot be deconstructed");
                }
            }
            Action::UnmarkDeconstruct { building } => {
                commands.entity(building).remove::<MarkedForDeconstruct>();
                for (_, crew, mut task, _, mut mov) in crews.iter_mut() {
                    if let CrewTask::Deconstruct(job) = &*task {
                        if job.target == building {
                            commands.entity(building).remove::<ReservedBy>();
                            let name = crew.name.clone();
                            *task = CrewTask::Idle(IdleCause::JobCanceled {
                                detail: "deconstruction unmarked".into(),
                            });
                            mov.path.clear();
                            log.push(
                                now,
                                LogKind::Fail,
                                format!("{name}: deconstruction canceled"),
                            );
                        }
                    }
                }
                log.push(now, LogKind::Info, "Deconstruction mark removed");
            }
            Action::SetRackFilter {
                rack,
                kind,
                allowed,
            } => {
                if let Ok((_, _, mut cell)) = racks.get_mut(rack) {
                    cell.allowed[kind.index()] = allowed;
                    log.push(
                        now,
                        LogKind::Info,
                        format!(
                            "Rack filter: {} {}",
                            if allowed { "allow" } else { "deny" },
                            kind.label()
                        ),
                    );
                }
            }
            Action::FabAddOrder { fab, batches } => {
                if let Ok((_, mut f)) = fabs.get_mut(fab) {
                    let next = match &f.order {
                        Some(o) => {
                            let mut o = *o;
                            o.batches = o.batches.saturating_add(batches);
                            o
                        }
                        None => crate::production::Order {
                            batches,
                            repeat: false,
                        },
                    };
                    f.order = Some(next);
                    log.push(now, LogKind::Info, "Fabricator order updated");
                }
            }
            Action::FabRepeat { fab } => {
                if let Ok((_, mut f)) = fabs.get_mut(fab) {
                    let next = match &f.order {
                        Some(o) => {
                            let mut o = *o;
                            o.repeat = !o.repeat;
                            o
                        }
                        None => crate::production::Order {
                            batches: 0,
                            repeat: true,
                        },
                    };
                    f.order = Some(next);
                    log.push(now, LogKind::Info, "Fabricator repeat toggled");
                }
            }
            Action::FabClearOrder { fab } => {
                if let Ok((_, mut f)) = fabs.get_mut(fab) {
                    f.order = None;
                    f.abort_cycle();
                    log.push(now, LogKind::Info, "Fabricator order cleared");
                }
            }
            Action::SetPriority { crew, work, level } => {
                for (e, mut c, _, _, _) in crews.iter_mut() {
                    if e == crew {
                        c.priorities.set(work, level);
                        log.push(
                            now,
                            LogKind::Info,
                            format!("{}: {} work -> {}", c.name, work.label(), level.label()),
                        );
                    }
                }
            }
            Action::SetGeneratorOn { gen, on } => {
                if let Ok((_, mut role)) = gens.get_mut(gen) {
                    if let PowerRole::Generator {
                        on: ref mut cur, ..
                    } = *role
                    {
                        *cur = on;
                        log.push(
                            now,
                            LogKind::Info,
                            format!("Reactor {}", if on { "online" } else { "standby" }),
                        );
                    }
                }
            }
            Action::MarkCableDeconstruct { pos } => {
                if !cables.has(pos) {
                    log.push(now, LogKind::Fail, "No cable there");
                } else if cable_tiles.iter().any(|(_, p)| *p == pos) {
                    // Already marked.
                } else {
                    let e = commands
                        .spawn((
                            TilePos::new(pos.x, pos.y),
                            Footprint::new(pos.x, pos.y, 1, 1),
                            Building {
                                kind: BuildingKind::PowerCable,
                                foot: Footprint::new(pos.x, pos.y, 1, 1),
                                demo_progress: 0.0,
                            },
                            MarkedForDeconstruct,
                        ))
                        .id();
                    let _ = e;
                    log.push(
                        now,
                        LogKind::Info,
                        format!("Cable at ({},{}) marked for removal", pos.x, pos.y),
                    );
                }
            }
            Action::MarkPipeDeconstruct { pos } => {
                if !pipes.has(pos) {
                    log.push(now, LogKind::Fail, "No coolant pipe there");
                } else if cable_tiles.iter().any(|(_, p)| *p == pos) {
                    // Already marked (the same transient-tile query covers
                    // pipe tiles: they are Building entities too).
                } else {
                    commands.spawn((
                        TilePos::new(pos.x, pos.y),
                        Footprint::new(pos.x, pos.y, 1, 1),
                        Building {
                            kind: BuildingKind::CoolantPipe,
                            foot: Footprint::new(pos.x, pos.y, 1, 1),
                            demo_progress: 0.0,
                        },
                        MarkedForDeconstruct,
                    ));
                    log.push(
                        now,
                        LogKind::Info,
                        format!("Coolant pipe at ({},{}) marked for removal", pos.x, pos.y),
                    );
                }
            }
            Action::CycleOverlay => {
                // Consumed by the UI plugin.
            }
            Action::SetSpeed { .. } | Action::TogglePause => {
                // Consumed by time_ctrl::speed_action_system.
            }
            Action::ToggleDebug => {
                // Consumed by ui::debug_toggle_system.
            }
            Action::SetTool { .. } => {
                // Consumed by the input plugin.
            }
        }
    }
}

/// The cargo hold region used by the debug spawn buttons (top-left room).
#[allow(clippy::type_complexity)]
fn random_cargo_tile(
    map: &ShipMap,
    items: &Query<(Entity, &TilePos, Option<&MarkedForHaul>), With<Item>>,
) -> Option<TilePos> {
    let occupied: HashSet<TilePos> = items.iter().map(|(_, p, _)| *p).collect();
    let mut candidates: Vec<TilePos> = (1..=10)
        .flat_map(|x| (1..=5).map(move |y| TilePos::new(x, y)))
        .filter(|p| map.is_walkable(*p) && !occupied.contains(p))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    // xorshift from wall-clock nanos; debug tool only.
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(0x9e3779b9)
        | 1;
    seed ^= seed << 13;
    seed ^= seed >> 7;
    seed ^= seed << 17;
    let idx = (seed as usize) % candidates.len();
    Some(candidates.swap_remove(idx))
}

/// Cancel a blueprint: refund on-site materials, end all jobs targeting it.
#[allow(clippy::too_many_arguments)]
fn cancel_blueprint(
    commands: &mut Commands,
    crews: &mut Query<(Entity, &mut Crew, &mut CrewTask, &TilePos, &mut Movement), Without<Item>>,
    map: &ShipMap,
    racks: &[(TilePos, Entity)],
    blueprint: Entity,
    delivered: [u32; 3],
    kind: BuildingKind,
    foot: Footprint,
    log: &mut EventLog,
    now: f64,
) {
    // End build jobs on it and supply hauls heading to it.
    for (_, crew, mut task, pos, mut mov) in crews.iter_mut() {
        let targeting = match &*task {
            CrewTask::Build(job) => job.target == blueprint,
            CrewTask::Haul(job) => job.dest == HaulDest::Blueprint(blueprint),
            _ => false,
        };
        if targeting {
            let name = crew.name.clone();
            if let CrewTask::Haul(hjob) = &*task {
                let item = hjob.item;
                end_haul(
                    &mut task,
                    &mut mov,
                    commands,
                    map,
                    racks,
                    *pos,
                    &name,
                    item,
                    log,
                    now,
                    IdleCause::JobCanceled {
                        detail: "blueprint canceled".into(),
                    },
                );
            } else {
                commands.entity(blueprint).remove::<ReservedBy>();
                *task = CrewTask::Idle(IdleCause::JobCanceled {
                    detail: "blueprint canceled".into(),
                });
                mov.path.clear();
                log.push(now, LogKind::Fail, format!("{name}: blueprint canceled"));
            }
        }
    }
    // Refund whatever had already been delivered to the site.
    let mut occupied: Vec<TilePos> = Vec::new();
    for kind_i in ItemKind::ALL {
        for _ in 0..delivered[kind_i.index()] {
            if let Some(t) =
                crate::map::find_drop_tile_ext(map, TilePos::new(foot.x, foot.y), &occupied)
            {
                occupied.push(t);
                crate::items::spawn_item(commands, t, kind_i);
            }
        }
    }
    commands.entity(blueprint).despawn();
    log.push(
        now,
        LogKind::Info,
        format!("{} blueprint canceled — materials refunded", kind.label()),
    );
}

/// End whatever job targets `item` (if any) and release its reservation.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn end_hauls_for_item(
    crews: &mut Query<(Entity, &mut Crew, &mut CrewTask, &TilePos, &mut Movement), Without<Item>>,
    commands: &mut Commands,
    map: &ShipMap,
    racks: &[(TilePos, Entity)],
    item: Entity,
    log: &mut EventLog,
    now: f64,
    detail: String,
) {
    for (_, crew, mut task, pos, mut mov) in crews.iter_mut() {
        if let CrewTask::Haul(job) = &*task {
            if job.item == item {
                let name = crew.name.clone();
                end_haul(
                    &mut task,
                    &mut mov,
                    commands,
                    map,
                    racks,
                    *pos,
                    &name,
                    item,
                    log,
                    now,
                    IdleCause::JobCanceled {
                        detail: detail.clone(),
                    },
                );
            }
        }
    }
    // A reservation without a matching job must not survive either.
    commands.entity(item).remove::<ReservedBy>();
}

/// Terminate one crew's haul job: release the claim, drop the item if it is
/// being carried, put the crew back to idle.
#[allow(clippy::too_many_arguments)]
fn end_haul(
    task: &mut CrewTask,
    mov: &mut Movement,
    commands: &mut Commands,
    map: &ShipMap,
    racks: &[(TilePos, Entity)],
    crew_pos: TilePos,
    crew_name: &str,
    item: Entity,
    log: &mut EventLog,
    now: f64,
    cause: IdleCause,
) {
    let carrying = match task {
        CrewTask::Haul(j) => matches!(j.phase, HaulPhase::ToDest | HaulPhase::Delivering),
        _ => false,
    };
    if carrying {
        let drop = find_drop_tile(map, crew_pos, racks).unwrap_or(crew_pos);
        commands.entity(item).insert(TilePos::new(drop.x, drop.y));
        commands
            .entity(item)
            .remove::<(CarriedBy, MarkedForHaul, ReservedBy)>();
        log.push(
            now,
            LogKind::Fail,
            format!(
                "{crew_name} dropped the item at ({},{}): {}",
                drop.x,
                drop.y,
                cause.label()
            ),
        );
    } else {
        commands.entity(item).remove::<(ReservedBy,)>();
        log.push(
            now,
            LogKind::Fail,
            format!("{crew_name}: {}", cause.label()),
        );
    }
    *task = CrewTask::Idle(cause);
    mov.path.clear();
    mov.progress = 0.0;
}

// =====================================================================================
// Job execution
// =====================================================================================

/// Racks that can take one item of `kind` right now.
fn racks_for_kind(
    racks: &Query<(Entity, &TilePos, &mut StorageCell), Without<Crew>>,
    kind: ItemKind,
) -> Vec<(Entity, TilePos)> {
    racks
        .iter()
        .filter(|(_, _, s)| s.can_take(kind))
        .map(|(e, p, _)| (e, *p))
        .collect()
}

/// Nearest reachable rack that accepts `kind`: (rack, path).
fn choose_rack(
    map: &ShipMap,
    from: TilePos,
    racks: &[(Entity, TilePos)],
) -> Option<(Entity, Vec<TilePos>)> {
    let mut sorted = racks.to_vec();
    sorted.sort_by(|(_, a), (_, b)| {
        crate::path::octile_distance(from, *a)
            .partial_cmp(&crate::path::octile_distance(from, *b))
            .unwrap()
    });
    for (e, p) in sorted {
        if let Some(path) = crate::path::find_path(map, from, p, |_| false) {
            return Some((e, path));
        }
    }
    None
}

/// Fail the crew's active haul job (shared by all error paths in the task system).
#[allow(clippy::too_many_arguments)]
fn fail_haul(
    commands: &mut Commands,
    map: &ShipMap,
    racks: &Query<(Entity, &TilePos, &mut StorageCell), Without<Crew>>,
    task: &mut CrewTask,
    mov: &mut Movement,
    crew: &mut Crew,
    crew_pos: TilePos,
    item: Entity,
    log: &mut EventLog,
    now: f64,
    detail: &str,
) {
    let rack_list: Vec<(TilePos, Entity)> = racks.iter().map(|(e, p, _)| (*p, e)).collect();
    let name = crew.name.clone();
    end_haul(
        task,
        mov,
        commands,
        map,
        &rack_list,
        crew_pos,
        &name,
        item,
        log,
        now,
        IdleCause::JobFailed {
            detail: detail.to_string(),
        },
    );
    crew.next_scan = now + RESCAN_FAILED as f64;
}

/// Whether a crew standing on `pos` can interact with `foot`.
fn at_interaction(map: &ShipMap, pos: TilePos, foot: &Footprint) -> bool {
    building::interaction_tiles(map, foot).contains(&pos)
}

/// Abort a work job cleanly (releases the reservation).
#[allow(clippy::too_many_arguments)]
fn end_work(
    commands: &mut Commands,
    task: &mut CrewTask,
    mov: &mut Movement,
    target: Entity,
    log: &mut EventLog,
    now: f64,
    name: &str,
    detail: &str,
) {
    commands.entity(target).remove::<ReservedBy>();
    *task = CrewTask::Idle(IdleCause::JobCanceled {
        detail: detail.to_string(),
    });
    mov.path.clear();
    mov.progress = 0.0;
    log.push(now, LogKind::Fail, format!("{name}: {detail}"));
}

/// Advance every active job through its phases.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
pub fn crew_task_system(
    mut map: ResMut<ShipMap>,
    mut cables: ResMut<CableGrid>,
    mut pipes: ResMut<crate::coolant::PipeGrid>,
    mut water: ResMut<crate::coolant::WaterGrid>,
    mut thermal: ResMut<crate::thermal::ThermalGrid>,
    mut cstats: ResMut<crate::coolant::CoolantStats>,
    clock: Res<SimClock>,
    mut log: ResMut<EventLog>,
    mut stats: ResMut<crate::stats::Stats>,
    mut commands: Commands,
    mut crews: Query<
        (
            Entity,
            &mut Crew,
            &mut CrewTask,
            &mut TilePos,
            &mut Movement,
        ),
        Without<Item>,
    >,
    items: Query<
        (
            Entity,
            &TilePos,
            &Item,
            Option<&MarkedForHaul>,
            Option<&CarriedBy>,
            Option<&ReservedBy>,
        ),
        With<Item>,
    >,
    mut racks: Query<(Entity, &TilePos, &mut StorageCell), Without<Crew>>,
    mut blueprints: Query<(Entity, &Footprint, &mut Blueprint), Without<Crew>>,
    mut buildings: Query<
        (
            Entity,
            &Footprint,
            &mut Building,
            Option<&MarkedForDeconstruct>,
        ),
        (Without<Crew>, Without<Blueprint>),
    >,
    mut fabs: Query<
        (
            Entity,
            &Footprint,
            &mut Fabricator,
            &PowerStatus,
            Option<&crate::thermal::ThermalState>,
        ),
        (Without<Crew>, Without<Blueprint>),
    >,
) {
    let dt = clock.dt() as f32;
    let now = clock.now();
    let crew_positions: Vec<(Entity, TilePos)> =
        crews.iter().map(|(e, _, _, p, _)| (e, *p)).collect();
    let ground_now: Vec<TilePos> = items.iter().map(|(_, p, ..)| *p).collect();

    for (crew_e, mut crew, mut task, pos, mut mov) in crews.iter_mut() {
        // ---- haul ------------------------------------------------------------
        if let CrewTask::Haul(job) = &mut *task {
            let item_entity = job.item;
            let Ok((_, item_pos, item, marked, carried, _)) = items.get(item_entity) else {
                let name = crew.name.clone();
                *task = CrewTask::Idle(IdleCause::JobCanceled {
                    detail: "target vanished".into(),
                });
                mov.path.clear();
                crew.next_scan = now + RESCAN_CANCELED as f64;
                log.push(
                    now,
                    LogKind::Fail,
                    format!("{name}: job canceled — target vanished"),
                );
                continue;
            };
            // Player-marked storage hauls die when the mark is removed; auto
            // hauls (blueprint/machine supply) have no mark to lose. An item
            // already in THIS crew's hands stays committed to its delivery
            // even without a mark — otherwise mid-flight conversions
            // (blueprint full → deliver to storage instead) would cancel
            // without dropping and leak the item in "carried" limbo forever.
            let needs_mark = matches!(job.dest, HaulDest::Storage) && carried.is_none();
            if needs_mark && marked.is_none() {
                let name = crew.name.clone();
                commands.entity(item_entity).remove::<ReservedBy>();
                *task = CrewTask::Idle(IdleCause::JobCanceled {
                    detail: "item unmarked".into(),
                });
                mov.path.clear();
                crew.next_scan = now + RESCAN_CANCELED as f64;
                log.push(
                    now,
                    LogKind::Fail,
                    format!("{name}: job canceled — item unmarked"),
                );
                continue;
            }
            if let Some(by) = carried {
                if by.0 != crew_e {
                    let name = crew.name.clone();
                    commands.entity(item_entity).remove::<ReservedBy>();
                    *task = CrewTask::Idle(IdleCause::JobCanceled {
                        detail: "item claimed elsewhere".into(),
                    });
                    mov.path.clear();
                    crew.next_scan = now + RESCAN_CANCELED as f64;
                    log.push(
                        now,
                        LogKind::Fail,
                        format!("{name}: job canceled — item claimed elsewhere"),
                    );
                    continue;
                }
            }

            match job.phase {
                HaulPhase::ToItem => {
                    if mov.path.is_empty() && *pos == *item_pos {
                        job.phase = HaulPhase::PickingUp;
                        job.timer = PICKUP_SECS;
                    } else if mov.path.is_empty() {
                        match crate::path::find_path(&map, *pos, *item_pos, |_| false) {
                            Some(p) => mov.path = p,
                            None => fail_haul(
                                &mut commands,
                                &map,
                                &racks,
                                &mut task,
                                &mut mov,
                                &mut crew,
                                *pos,
                                item_entity,
                                &mut log,
                                now,
                                "path to item lost",
                            ),
                        }
                    }
                }
                HaulPhase::PickingUp => {
                    job.timer -= dt;
                    if job.timer <= 0.0 {
                        if *pos == *item_pos {
                            commands.entity(item_entity).insert(CarriedBy(crew_e));
                            match &job.dest {
                                HaulDest::Storage => {
                                    match choose_rack(
                                        &map,
                                        *pos,
                                        &racks_for_kind(&racks, item.kind),
                                    ) {
                                        Some((rack, path)) => {
                                            job.target_rack = Some(rack);
                                            job.phase = HaulPhase::ToDest;
                                            mov.path = path;
                                        }
                                        None => {
                                            job.phase = HaulPhase::ToDest;
                                            fail_haul(
                                                &mut commands,
                                                &map,
                                                &racks,
                                                &mut task,
                                                &mut mov,
                                                &mut crew,
                                                *pos,
                                                item_entity,
                                                &mut log,
                                                now,
                                                "no reachable storage with space",
                                            );
                                        }
                                    }
                                }
                                HaulDest::Blueprint(bp_e) => {
                                    let ok = blueprints
                                        .get(*bp_e)
                                        .ok()
                                        .filter(|(_, _, bp)| bp.missing(item.kind) > 0)
                                        .and_then(|(_, foot, _)| {
                                            building::path_to_interaction(&map, *pos, foot)
                                        });
                                    match ok {
                                        Some(path) => {
                                            job.phase = HaulPhase::ToDest;
                                            mov.path = path;
                                        }
                                        None => {
                                            // Blueprint gone or no longer needs this
                                            // material: take the item to storage instead.
                                            job.dest = HaulDest::Storage;
                                            job.phase = HaulPhase::ToDest;
                                            match choose_rack(
                                                &map,
                                                *pos,
                                                &racks_for_kind(&racks, item.kind),
                                            ) {
                                                Some((rack, path)) => {
                                                    job.target_rack = Some(rack);
                                                    mov.path = path;
                                                }
                                                None => fail_haul(
                                                    &mut commands,
                                                    &map,
                                                    &racks,
                                                    &mut task,
                                                    &mut mov,
                                                    &mut crew,
                                                    *pos,
                                                    item_entity,
                                                    &mut log,
                                                    now,
                                                    "blueprint gone and no storage",
                                                ),
                                            }
                                        }
                                    }
                                }
                                HaulDest::Machine(fab_e) => {
                                    let ok =
                                        fabs.get(*fab_e).ok().and_then(|(_, foot, _, _, _)| {
                                            building::path_to_interaction(&map, *pos, foot)
                                        });
                                    match ok {
                                        Some(path) => {
                                            job.phase = HaulPhase::ToDest;
                                            mov.path = path;
                                        }
                                        None => {
                                            job.dest = HaulDest::Storage;
                                            job.phase = HaulPhase::ToDest;
                                            match choose_rack(
                                                &map,
                                                *pos,
                                                &racks_for_kind(&racks, item.kind),
                                            ) {
                                                Some((rack, path)) => {
                                                    job.target_rack = Some(rack);
                                                    mov.path = path;
                                                }
                                                None => fail_haul(
                                                    &mut commands,
                                                    &map,
                                                    &racks,
                                                    &mut task,
                                                    &mut mov,
                                                    &mut crew,
                                                    *pos,
                                                    item_entity,
                                                    &mut log,
                                                    now,
                                                    "machine gone and no storage",
                                                ),
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            // Item ended up elsewhere: walk to it again.
                            job.phase = HaulPhase::ToItem;
                        }
                    }
                }
                HaulPhase::ToDest => match job.dest {
                    HaulDest::Storage => {
                        let Some(rack_e) = job.target_rack else {
                            match choose_rack(&map, *pos, &racks_for_kind(&racks, item.kind)) {
                                Some((rack, path)) => {
                                    job.target_rack = Some(rack);
                                    mov.path = path;
                                }
                                None => fail_haul(
                                    &mut commands,
                                    &map,
                                    &racks,
                                    &mut task,
                                    &mut mov,
                                    &mut crew,
                                    *pos,
                                    item_entity,
                                    &mut log,
                                    now,
                                    "no reachable storage with space",
                                ),
                            }
                            continue;
                        };
                        let Ok((_, rack_pos, rack)) = racks.get(rack_e) else {
                            match choose_rack(&map, *pos, &racks_for_kind(&racks, item.kind)) {
                                Some((rack, path)) => {
                                    job.target_rack = Some(rack);
                                    mov.path = path;
                                }
                                None => fail_haul(
                                    &mut commands,
                                    &map,
                                    &racks,
                                    &mut task,
                                    &mut mov,
                                    &mut crew,
                                    *pos,
                                    item_entity,
                                    &mut log,
                                    now,
                                    "storage disappeared",
                                ),
                            }
                            continue;
                        };
                        let rack_pos = *rack_pos;
                        let rack_free = rack.can_take(item.kind);
                        if mov.path.is_empty() && *pos == rack_pos {
                            if rack_free {
                                job.phase = HaulPhase::Delivering;
                                job.timer = DELIVER_SECS;
                            } else {
                                match choose_rack(&map, *pos, &racks_for_kind(&racks, item.kind)) {
                                    Some((rack, path)) => {
                                        job.target_rack = Some(rack);
                                        mov.path = path;
                                    }
                                    None => fail_haul(
                                        &mut commands,
                                        &map,
                                        &racks,
                                        &mut task,
                                        &mut mov,
                                        &mut crew,
                                        *pos,
                                        item_entity,
                                        &mut log,
                                        now,
                                        "no free storage space",
                                    ),
                                }
                            }
                        } else if mov.path.is_empty() {
                            match crate::path::find_path(&map, *pos, rack_pos, |_| false) {
                                Some(p) => mov.path = p,
                                None => fail_haul(
                                    &mut commands,
                                    &map,
                                    &racks,
                                    &mut task,
                                    &mut mov,
                                    &mut crew,
                                    *pos,
                                    item_entity,
                                    &mut log,
                                    now,
                                    "path to storage lost",
                                ),
                            }
                        }
                    }
                    HaulDest::Blueprint(bp_e) => {
                        let Ok((_, foot, _)) = blueprints.get(bp_e) else {
                            job.dest = HaulDest::Storage;
                            job.target_rack = None;
                            continue;
                        };
                        let foot = *foot;
                        if mov.path.is_empty() && at_interaction(&map, *pos, &foot) {
                            job.phase = HaulPhase::Delivering;
                            job.timer = DELIVER_SECS;
                        } else if mov.path.is_empty() {
                            match building::path_to_interaction(&map, *pos, &foot) {
                                Some(p) => mov.path = p,
                                None => fail_haul(
                                    &mut commands,
                                    &map,
                                    &racks,
                                    &mut task,
                                    &mut mov,
                                    &mut crew,
                                    *pos,
                                    item_entity,
                                    &mut log,
                                    now,
                                    "path to blueprint lost",
                                ),
                            }
                        }
                    }
                    HaulDest::Machine(fab_e) => {
                        let Ok((_, foot, _, _, _)) = fabs.get(fab_e) else {
                            job.dest = HaulDest::Storage;
                            job.target_rack = None;
                            continue;
                        };
                        let foot = *foot;
                        if mov.path.is_empty() && at_interaction(&map, *pos, &foot) {
                            job.phase = HaulPhase::Delivering;
                            job.timer = DELIVER_SECS;
                        } else if mov.path.is_empty() {
                            match building::path_to_interaction(&map, *pos, &foot) {
                                Some(p) => mov.path = p,
                                None => fail_haul(
                                    &mut commands,
                                    &map,
                                    &racks,
                                    &mut task,
                                    &mut mov,
                                    &mut crew,
                                    *pos,
                                    item_entity,
                                    &mut log,
                                    now,
                                    "path to machine lost",
                                ),
                            }
                        }
                    }
                },
                HaulPhase::Delivering => {
                    job.timer -= dt;
                    if job.timer <= 0.0 {
                        match job.dest {
                            HaulDest::Storage => {
                                let Some(rack_e) = job.target_rack else {
                                    job.phase = HaulPhase::ToDest;
                                    continue;
                                };
                                let Ok((_, rack_pos, mut rack_cell)) = racks.get_mut(rack_e) else {
                                    fail_haul(
                                        &mut commands,
                                        &map,
                                        &racks,
                                        &mut task,
                                        &mut mov,
                                        &mut crew,
                                        *pos,
                                        item_entity,
                                        &mut log,
                                        now,
                                        "storage disappeared",
                                    );
                                    continue;
                                };
                                if *pos == *rack_pos && rack_cell.try_add(item.kind) {
                                    crew.delivered += 1;
                                    stats.hauls_done += 1;
                                    let name = crew.name.clone();
                                    let kind = item.kind.label();
                                    log.push(now, LogKind::Job, format!("{name} stored {kind}"));
                                    commands.entity(item_entity).despawn();
                                    *task = CrewTask::Idle(IdleCause::Looking);
                                    crew.next_scan = now;
                                } else {
                                    job.phase = HaulPhase::ToDest;
                                }
                            }
                            HaulDest::Blueprint(bp_e) => {
                                if let Ok((_, _, mut bp)) = blueprints.get_mut(bp_e) {
                                    bp.delivered[item.kind.index()] += 1;
                                    crew.delivered += 1;
                                    let name = crew.name.clone();
                                    let kind = item.kind.label();
                                    log.push(
                                        now,
                                        LogKind::Job,
                                        format!("{name} delivered {kind} to blueprint"),
                                    );
                                    commands.entity(item_entity).despawn();
                                    *task = CrewTask::Idle(IdleCause::Looking);
                                    crew.next_scan = now;
                                } else {
                                    job.dest = HaulDest::Storage;
                                    job.phase = HaulPhase::ToDest;
                                }
                            }
                            HaulDest::Machine(fab_e) => {
                                if let Ok((_, _, mut f, _, _)) = fabs.get_mut(fab_e) {
                                    f.input[item.kind.index()] += 1;
                                    crew.delivered += 1;
                                    let name = crew.name.clone();
                                    let kind = item.kind.label();
                                    log.push(
                                        now,
                                        LogKind::Job,
                                        format!("{name} loaded {kind} into fabricator"),
                                    );
                                    commands.entity(item_entity).despawn();
                                    *task = CrewTask::Idle(IdleCause::Looking);
                                    crew.next_scan = now;
                                } else {
                                    job.dest = HaulDest::Storage;
                                    job.phase = HaulPhase::ToDest;
                                }
                            }
                        }
                    }
                }
            }
            continue;
        }

        // ---- construct -------------------------------------------------------
        if let CrewTask::Build(job) = &mut *task {
            let Ok((_, foot, mut bp)) = blueprints.get_mut(job.target) else {
                let name = crew.name.clone();
                *task = CrewTask::Idle(IdleCause::JobCanceled {
                    detail: "blueprint gone".into(),
                });
                mov.path.clear();
                crew.next_scan = now + RESCAN_CANCELED as f64;
                log.push(now, LogKind::Fail, format!("{name}: blueprint gone"));
                continue;
            };
            let foot = *foot;
            let phase = job.phase;
            match phase {
                WorkPhase::Going => {
                    if mov.path.is_empty() && at_interaction(&map, *pos, &foot) {
                        job.phase = WorkPhase::Working;
                        job.timer = building::def(bp.kind).work_secs;
                        job.total = job.timer.max(0.001);
                    } else if mov.path.is_empty() {
                        match building::path_to_interaction(&map, *pos, &foot) {
                            Some(p) => mov.path = p,
                            None => {
                                let name = crew.name.clone();
                                let target = job.target;
                                end_work(
                                    &mut commands,
                                    &mut task,
                                    &mut mov,
                                    target,
                                    &mut log,
                                    now,
                                    &name,
                                    "no path to blueprint",
                                );
                                crew.next_scan = now + RESCAN_UNREACHABLE as f64;
                            }
                        }
                    }
                }
                WorkPhase::Working => {
                    if !at_interaction(&map, *pos, &foot) {
                        job.phase = WorkPhase::Going;
                        continue;
                    }
                    job.timer -= dt;
                    bp.progress = 1.0 - (job.timer / job.total).clamp(0.0, 1.0);
                    if job.timer <= 0.0 {
                        building::complete_building(
                            &mut commands,
                            &mut map,
                            &mut cables,
                            &mut pipes,
                            &mut thermal,
                            job.target,
                            &bp,
                            &crew_positions,
                            &ground_now,
                            &mut log,
                            &mut stats,
                            now,
                        );
                        crew.built += 1;
                        *task = CrewTask::Idle(IdleCause::Looking);
                        crew.next_scan = now;
                    }
                }
            }
            continue;
        }

        // ---- deconstruct -----------------------------------------------------
        if let CrewTask::Deconstruct(job) = &mut *task {
            let Ok((_, foot, mut b, marked)) = buildings.get_mut(job.target) else {
                let name = crew.name.clone();
                *task = CrewTask::Idle(IdleCause::JobCanceled {
                    detail: "building gone".into(),
                });
                mov.path.clear();
                crew.next_scan = now + RESCAN_CANCELED as f64;
                log.push(now, LogKind::Fail, format!("{name}: building gone"));
                continue;
            };
            let foot = *foot;
            if marked.is_none() {
                let name = crew.name.clone();
                let target = job.target;
                end_work(
                    &mut commands,
                    &mut task,
                    &mut mov,
                    target,
                    &mut log,
                    now,
                    &name,
                    "deconstruction unmarked",
                );
                crew.next_scan = now + RESCAN_CANCELED as f64;
                continue;
            }
            let phase = job.phase;
            match phase {
                WorkPhase::Going => {
                    if mov.path.is_empty() && at_interaction(&map, *pos, &foot) {
                        job.phase = WorkPhase::Working;
                        job.timer = building::def(b.kind).demo_secs;
                        job.total = job.timer.max(0.001);
                    } else if mov.path.is_empty() {
                        match building::path_to_interaction(&map, *pos, &foot) {
                            Some(p) => mov.path = p,
                            None => {
                                let name = crew.name.clone();
                                let target = job.target;
                                end_work(
                                    &mut commands,
                                    &mut task,
                                    &mut mov,
                                    target,
                                    &mut log,
                                    now,
                                    &name,
                                    "no path to building",
                                );
                                crew.next_scan = now + RESCAN_UNREACHABLE as f64;
                            }
                        }
                    }
                }
                WorkPhase::Working => {
                    if !at_interaction(&map, *pos, &foot) {
                        job.phase = WorkPhase::Going;
                        continue;
                    }
                    job.timer -= dt;
                    b.demo_progress = 1.0 - (job.timer / job.total).clamp(0.0, 1.0);
                    if job.timer <= 0.0 {
                        let rack_contents: Option<[u32; 3]> =
                            racks.get(job.target).ok().map(|(_, _, cell)| cell.counts);
                        building::complete_deconstruction(
                            &mut commands,
                            &mut map,
                            &mut cables,
                            &mut pipes,
                            &mut water,
                            &mut cstats,
                            &mut thermal,
                            job.target,
                            &b,
                            rack_contents,
                            &ground_now,
                            &mut log,
                            &mut stats,
                            now,
                        );
                        crew.built += 1;
                        *task = CrewTask::Idle(IdleCause::Looking);
                        crew.next_scan = now;
                    }
                }
            }
            continue;
        }

        // ---- operate ---------------------------------------------------------
        if let CrewTask::Operate(job) = &mut *task {
            let Ok((_, foot, mut f, power, thermal_state)) = fabs.get_mut(job.target) else {
                let name = crew.name.clone();
                *task = CrewTask::Idle(IdleCause::JobCanceled {
                    detail: "machine gone".into(),
                });
                mov.path.clear();
                crew.next_scan = now + RESCAN_CANCELED as f64;
                log.push(now, LogKind::Fail, format!("{name}: machine gone"));
                continue;
            };
            let foot = *foot;
            let phase = job.phase;
            match phase {
                WorkPhase::Going => {
                    // The order may have been cleared or power lost on the way.
                    if !f.ready_to_work() || !power.ok() {
                        let name = crew.name.clone();
                        let target = job.target;
                        end_work(
                            &mut commands,
                            &mut task,
                            &mut mov,
                            target,
                            &mut log,
                            now,
                            &name,
                            "order canceled while walking",
                        );
                        crew.next_scan = now + RESCAN_CANCELED as f64;
                        continue;
                    }
                    if mov.path.is_empty() && at_interaction(&map, *pos, &foot) {
                        job.phase = WorkPhase::Working;
                        job.timer = crate::production::RECIPE.work_secs;
                        job.total = job.timer;
                        f.active = true;
                    } else if mov.path.is_empty() {
                        match building::path_to_interaction(&map, *pos, &foot) {
                            Some(p) => mov.path = p,
                            None => {
                                let name = crew.name.clone();
                                let target = job.target;
                                end_work(
                                    &mut commands,
                                    &mut task,
                                    &mut mov,
                                    target,
                                    &mut log,
                                    now,
                                    &name,
                                    "no path to machine",
                                );
                                crew.next_scan = now + RESCAN_UNREACHABLE as f64;
                            }
                        }
                    }
                }
                WorkPhase::Working => {
                    if !power.ok() {
                        // Power lost mid-cycle: abort without consuming ore.
                        f.abort_cycle();
                        let name = crew.name.clone();
                        let target = job.target;
                        end_work(
                            &mut commands,
                            &mut task,
                            &mut mov,
                            target,
                            &mut log,
                            now,
                            &name,
                            "power lost",
                        );
                        crew.next_scan = now + RESCAN_FAILED as f64;
                        continue;
                    }
                    if !f.active || !at_interaction(&map, *pos, &foot) {
                        // Someone cleared the order or we got displaced.
                        f.abort_cycle();
                        let name = crew.name.clone();
                        let target = job.target;
                        end_work(
                            &mut commands,
                            &mut task,
                            &mut mov,
                            target,
                            &mut log,
                            now,
                            &name,
                            "production interrupted",
                        );
                        crew.next_scan = now + RESCAN_FAILED as f64;
                        continue;
                    }
                    // Thermal derating: an overheated machine works slower, a
                    // critical one stalls (the operator stays, progress
                    // freezes — visible in the machine panel).
                    let thermal_scale = thermal_state.map(|s| s.work_factor()).unwrap_or(1.0);
                    job.timer -= dt * thermal_scale;
                    f.progress = 1.0 - (job.timer / job.total).clamp(0.0, 1.0);
                    if job.timer <= 0.0 {
                        let out_kind = f.finish_cycle();
                        stats.produced += 1;
                        crew.operated += 1;
                        let name = crew.name.clone();
                        log.push(
                            now,
                            LogKind::Job,
                            format!("{name} produced {} at the fabricator", out_kind.label()),
                        );
                        commands.entity(job.target).remove::<ReservedBy>();
                        *task = CrewTask::Idle(IdleCause::Looking);
                        crew.next_scan = now;
                    }
                }
            }
            continue;
        }
    }
}

// =====================================================================================
// Job claiming (unified scan across all work types)
// =====================================================================================

/// One claimable unit of work, generated per idle crew from live world state.
struct Candidate {
    prio: Priority,
    dist: f32,
    cand: Cand,
}

impl Candidate {
    /// Higher is better: priority tier dominates, distance breaks ties inside
    /// a tier (capped so a far High job still beats a near Low job).
    fn score(&self) -> i32 {
        self.prio.weight() - (self.dist.min(60.0) as i32)
    }
}

enum Cand {
    /// A ground item (player-marked → storage, free → auto demand).
    Ground {
        item: Entity,
        dest: HaulDest,
    },
    /// Pull one unit of `kind` out of a rack for an auto demand.
    RackPull {
        rack: Entity,
        kind: ItemKind,
        dest: HaulDest,
    },
    /// Pull one unit of `kind` out of a fabricator's output buffer.
    MachineOut {
        fab: Entity,
        kind: ItemKind,
    },
    Build {
        bp: Entity,
    },
    Demo {
        building: Entity,
    },
    Operate {
        fab: Entity,
    },
}

/// Nearest usable source (ground item preferred, rack stock with +1 distance
/// penalty so racks act as reserves) for one auto-logistics demand.
#[allow(clippy::type_complexity)]
fn best_source_for(
    crew_pos: TilePos,
    kind: ItemKind,
    items: &Query<
        (
            Entity,
            &TilePos,
            &Item,
            Option<&ReservedBy>,
            Option<&CarriedBy>,
            Option<&NoPathUntil>,
            Option<&MarkedForHaul>,
        ),
        With<Item>,
    >,
    racks: &Query<(Entity, &TilePos, &mut StorageCell), Without<Crew>>,
    local_claims: &HashSet<Entity>,
    now: f64,
    dest: HaulDest,
) -> Option<(f32, Cand)> {
    let mut best: Option<(f32, Cand)> = None;
    for (e, p, it, reserved, carried, cooled, marked) in items.iter() {
        if it.kind != kind
            || marked.is_some() // player storage intent wins over auto demands
            || reserved.is_some()
            || carried.is_some()
            || local_claims.contains(&e)
        {
            continue;
        }
        if cooled.is_some_and(|c| c.0 > now) {
            continue;
        }
        let d = crate::path::octile_distance(crew_pos, *p);
        if best.as_ref().is_none_or(|(bd, _)| d < *bd) {
            best = Some((d, Cand::Ground { item: e, dest }));
        }
    }
    for (e, p, cell) in racks.iter() {
        if !cell.has_kind(kind) {
            continue;
        }
        // +1 keeps the tie-break preference for loose ground items over
        // rack stock (racks act as reserves).
        let d = crate::path::octile_distance(crew_pos, *p) + 1.0;
        if best.as_ref().is_none_or(|(bd, _)| d < *bd) {
            best = Some((
                d,
                Cand::RackPull {
                    rack: e,
                    kind,
                    dest,
                },
            ));
        }
    }
    best
}

/// Idle crew scan all three work categories for claimable jobs and take the
/// best one by (priority tier, then distance). Claims are exclusive: item /
/// blueprint / building / machine reservations plus an in-frame local set so
/// two idle crews in the same frame never grab the same work.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
pub fn crew_scan_system(
    map: Res<ShipMap>,
    clock: Res<SimClock>,
    mut log: ResMut<EventLog>,
    mut stats: ResMut<crate::stats::Stats>,
    mut commands: Commands,
    mut crews: Query<(Entity, &mut Crew, &mut CrewTask, &TilePos, &mut Movement), Without<Item>>,
    items: Query<
        (
            Entity,
            &TilePos,
            &Item,
            Option<&ReservedBy>,
            Option<&CarriedBy>,
            Option<&NoPathUntil>,
            Option<&MarkedForHaul>,
        ),
        With<Item>,
    >,
    mut racks: Query<(Entity, &TilePos, &mut StorageCell), Without<Crew>>,
    blueprints: Query<(Entity, &Footprint, &Blueprint, Option<&ReservedBy>), Without<Crew>>,
    buildings: Query<
        (
            Entity,
            &Footprint,
            Option<&MarkedForDeconstruct>,
            Option<&ReservedBy>,
        ),
        (With<Building>, Without<Crew>),
    >,
    mut fabs: Query<
        (
            Entity,
            &Footprint,
            &mut Fabricator,
            Option<&ReservedBy>,
            &PowerStatus,
        ),
        (Without<Crew>, Without<Blueprint>),
    >,
) {
    let now = clock.now();

    // Inbound supply: how many haulers are already en route to each consumer.
    let mut inbound: HashMap<(Entity, usize), u32> = HashMap::new();
    for (_, _, task, _, _) in crews.iter() {
        if let CrewTask::Haul(job) = task {
            let dest_e = match job.dest {
                HaulDest::Blueprint(e) | HaulDest::Machine(e) => Some(e),
                HaulDest::Storage => None,
            };
            if let Some(de) = dest_e {
                if let Ok((_, _, it, _, _, _, _)) = items.get(job.item) {
                    *inbound.entry((de, it.kind.index())).or_insert(0) += 1;
                }
            }
        }
    }

    let ground_snapshot: Vec<TilePos> = items.iter().map(|(_, p, ..)| *p).collect();
    // Entities claimed this frame (commands are deferred; this set closes the gap).
    let mut local_claims: HashSet<Entity> = HashSet::new();

    for (crew_e, mut crew, mut task, pos, mut mov) in crews.iter_mut() {
        if std::env::var("SLICE0_SCAN_DEBUG").is_ok() && !matches!(*task, CrewTask::Idle(_)) {
            if let CrewTask::Haul(j) = &*task {
                let item_info = items
                    .get(j.item)
                    .map(|(_, p, it, ..)| format!("kind={:?} pos=({},{})", it.kind, p.x, p.y))
                    .unwrap_or_else(|_| "GONE".into());
                println!(
                    "JOB_DEBUG crew={:?} pos=({},{}) path={} prog={:.2} phase={:?} dest={:?} item={}",
                    crew.name, pos.x, pos.y, mov.path.len(), mov.progress, j.phase, j.dest, item_info
                );
            }
        }
        if !matches!(*task, CrewTask::Idle(_)) || now < crew.next_scan {
            continue;
        }

        let haul_prio = crew.priorities.get(WorkKind::Haul);
        let build_prio = crew.priorities.get(WorkKind::Build);
        let operate_prio = crew.priorities.get(WorkKind::Operate);

        let mut candidates: Vec<Candidate> = Vec::new();
        let mut marked_exists = false;
        let mut marked_any_free = false;
        let mut storage_ok = false;

        // ---- haul candidates -------------------------------------------------
        if haul_prio != Priority::Disabled {
            // (a) player-marked ground items → storage.
            for (e, p, it, reserved, carried, cooled, marked) in items.iter() {
                if marked.is_none() {
                    continue;
                }
                marked_exists = true;
                if reserved.is_some() || carried.is_some() || local_claims.contains(&e) {
                    continue;
                }
                // Free-but-cooled items still count as "free" for idle-cause
                // reporting; they only fail the claim attempt itself.
                marked_any_free = true;
                if cooled.is_some_and(|c| c.0 > now) {
                    continue;
                }
                if racks.iter().any(|(_, _, s)| s.can_take(it.kind)) {
                    storage_ok = true;
                }
                if !racks.iter().any(|(_, _, s)| s.can_take(it.kind)) {
                    continue;
                }
                let d = crate::path::octile_distance(*pos, *p);
                candidates.push(Candidate {
                    prio: haul_prio,
                    dist: d,
                    cand: Cand::Ground {
                        item: e,
                        dest: HaulDest::Storage,
                    },
                });
            }
            // (b) blueprint material demands.
            for (e, _, bp, _) in blueprints.iter() {
                for (kind, miss) in bp.missing_list() {
                    let already = inbound.get(&(e, kind.index())).copied().unwrap_or(0);
                    if already >= miss {
                        continue;
                    }
                    if let Some((d, cand)) = best_source_for(
                        *pos,
                        kind,
                        &items,
                        &racks,
                        &local_claims,
                        now,
                        HaulDest::Blueprint(e),
                    ) {
                        candidates.push(Candidate {
                            prio: haul_prio,
                            dist: d,
                            cand,
                        });
                    }
                }
            }
            // (c) fabricator input demands.
            for (e, _, f, _, _) in fabs.iter() {
                let in_kind = crate::production::RECIPE.in_kind;
                let want = f.input_want(inbound.get(&(e, in_kind.index())).copied().unwrap_or(0));
                if want > 0 {
                    if let Some((d, cand)) = best_source_for(
                        *pos,
                        in_kind,
                        &items,
                        &racks,
                        &local_claims,
                        now,
                        HaulDest::Machine(e),
                    ) {
                        candidates.push(Candidate {
                            prio: haul_prio,
                            dist: d,
                            cand,
                        });
                    }
                }
            }
            // (d) fabricator output → nearest accepting rack.
            for (e, foot, f, _, _) in fabs.iter() {
                for kind in ItemKind::ALL {
                    if f.output[kind.index()] == 0 {
                        continue;
                    }
                    if !racks.iter().any(|(_, _, s)| s.can_take(kind)) {
                        continue;
                    }
                    candidates.push(Candidate {
                        prio: haul_prio,
                        dist: foot.distance_to(*pos),
                        cand: Cand::MachineOut { fab: e, kind },
                    });
                }
            }
        }

        // ---- build candidates ------------------------------------------------
        if build_prio != Priority::Disabled {
            for (e, foot, bp, res) in blueprints.iter() {
                if bp.fully_supplied() && res.is_none() && !local_claims.contains(&e) {
                    candidates.push(Candidate {
                        prio: build_prio,
                        dist: foot.distance_to(*pos),
                        cand: Cand::Build { bp: e },
                    });
                }
            }
            for (e, foot, marked, res) in buildings.iter() {
                if marked.is_some() && res.is_none() && !local_claims.contains(&e) {
                    candidates.push(Candidate {
                        prio: build_prio,
                        dist: foot.distance_to(*pos),
                        cand: Cand::Demo { building: e },
                    });
                }
            }
        }

        // ---- operate candidates ----------------------------------------------
        if operate_prio != Priority::Disabled {
            for (e, foot, f, res, power) in fabs.iter() {
                if f.ready_to_work() && power.ok() && res.is_none() && !local_claims.contains(&e) {
                    candidates.push(Candidate {
                        prio: operate_prio,
                        dist: foot.distance_to(*pos),
                        cand: Cand::Operate { fab: e },
                    });
                }
            }
        }

        if candidates.is_empty() && std::env::var("SLICE0_SCAN_DEBUG").is_ok() {
            let fab_info: Vec<String> = fabs
                .iter()
                .map(|(e, _, f, _, _)| format!("e={e:?} want={}", f.input_want(0)))
                .collect();
            let unmarked_ground: Vec<&TilePos> = items
                .iter()
                .filter(|(_, _, _, r, c, _, m)| r.is_none() && c.is_none() && m.is_none())
                .map(|(_, p, ..)| p)
                .collect();
            println!(
                "SCAN_DEBUG crew={:?} pos=({},{}) fabs={fab_info:?} unmarked_ground_n={} marked={}",
                crew.name,
                pos.x,
                pos.y,
                unmarked_ground.len(),
                marked_exists
            );
        }
        if candidates.is_empty() {
            let cause = if haul_prio == Priority::Disabled
                && build_prio == Priority::Disabled
                && operate_prio == Priority::Disabled
            {
                IdleCause::AllWorkDisabled
            } else if haul_prio == Priority::Disabled {
                // Haul work exists but this crew refuses it; other categories
                // had no candidates either.
                IdleCause::NothingToDo
            } else if marked_any_free {
                if !storage_ok {
                    IdleCause::NoStorageSpace
                } else {
                    IdleCause::AllUnreachable
                }
            } else if marked_exists {
                IdleCause::AllClaimed
            } else {
                IdleCause::NothingToDo
            };
            *task = CrewTask::Idle(cause);
            crew.next_scan = now + SCAN_IDLE as f64;
            continue;
        }

        // ---- pick and claim ----------------------------------------------------
        candidates.sort_by(|a, b| {
            b.score()
                .cmp(&a.score())
                .then(a.dist.partial_cmp(&b.dist).unwrap())
        });
        let mut claimed = false;
        for cand in &candidates {
            match cand.cand {
                Cand::Ground { item, dest } => {
                    let Ok((_, item_pos, it, _, _, _cooled, _)) = items.get(item) else {
                        continue;
                    };
                    let bp_lookup = |e: Entity| blueprints.get(e).ok().map(|(_, f, _, _)| *f);
                    let fab_lookup = |e: Entity| fabs.get(e).ok().map(|(_, f, ..)| *f);
                    if !dest_reachable(&map, *item_pos, dest, bp_lookup, fab_lookup) {
                        continue;
                    }
                    match crate::path::find_path(&map, *pos, *item_pos, |_| false) {
                        Some(path) => {
                            commands.entity(item).insert(ReservedBy(crew_e));
                            local_claims.insert(item);
                            stats.haul_distance += crate::path::path_length(Some(*pos), &path);
                            set_haul_task(&mut task, &mut mov, item, dest, path, crew_e);
                            let name = crew.name.clone();
                            let dest_label = match dest {
                                HaulDest::Storage => "storage".to_string(),
                                HaulDest::Blueprint(_) => "a blueprint".to_string(),
                                HaulDest::Machine(_) => "the fabricator".to_string(),
                            };
                            log.push(
                                now,
                                LogKind::Info,
                                format!(
                                    "{} claimed {} at ({},{}) for {}",
                                    name,
                                    it.kind.label(),
                                    item_pos.x,
                                    item_pos.y,
                                    dest_label
                                ),
                            );
                            claimed = true;
                        }
                        None => {
                            // Only player-marked storage hauls get a cooldown entry.
                            if matches!(dest, HaulDest::Storage) {
                                commands
                                    .entity(item)
                                    .insert(NoPathUntil(now + EventLog::UNREACHABLE_COOLDOWN));
                                log.push(
                                    now,
                                    LogKind::Fail,
                                    format!(
                                        "{} at ({},{}) is unreachable",
                                        it.kind.label(),
                                        item_pos.x,
                                        item_pos.y
                                    ),
                                );
                            }
                        }
                    }
                }
                Cand::RackPull { rack, kind, dest } => {
                    let Ok((_, rack_pos, mut cell)) = racks.get_mut(rack) else {
                        continue;
                    };
                    if !cell.take(kind) {
                        continue;
                    }
                    let rack_pos = *rack_pos;
                    let bp_lookup = |e: Entity| blueprints.get(e).ok().map(|(_, f, _, _)| *f);
                    let fab_lookup = |e: Entity| fabs.get(e).ok().map(|(_, f, ..)| *f);
                    if !dest_reachable(&map, rack_pos, dest, bp_lookup, fab_lookup) {
                        // Unreachable consumer: put the unit back, no claim.
                        cell.counts[kind.index()] += 1;
                        continue;
                    }
                    match crate::path::find_path(&map, *pos, rack_pos, |_| false) {
                        Some(path) => {
                            let item = crate::items::spawn_item(&mut commands, rack_pos, kind);
                            commands.entity(item).insert(ReservedBy(crew_e));
                            local_claims.insert(item);
                            if let Some(de) = dest_entity(dest) {
                                *inbound.entry((de, kind.index())).or_insert(0) += 1;
                            }
                            stats.haul_distance += crate::path::path_length(Some(*pos), &path);
                            set_haul_task(&mut task, &mut mov, item, dest, path, crew_e);
                            let name = crew.name.clone();
                            log.push(
                                now,
                                LogKind::Info,
                                format!(
                                    "{} fetched {} from the rack at ({},{})",
                                    name,
                                    kind.label(),
                                    rack_pos.x,
                                    rack_pos.y
                                ),
                            );
                            claimed = true;
                        }
                        None => {
                            // Unreachable rack: put the unit back.
                            cell.counts[kind.index()] += 1;
                        }
                    }
                }
                Cand::MachineOut { fab, kind } => {
                    let Ok((_, foot, mut f, _, _)) = fabs.get_mut(fab) else {
                        continue;
                    };
                    if f.output[kind.index()] == 0 {
                        continue;
                    }
                    f.output[kind.index()] -= 1;
                    let Some(tile) = crate::map::find_drop_tile_ext(
                        &map,
                        TilePos::new(foot.x, foot.y),
                        &ground_snapshot,
                    ) else {
                        f.output[kind.index()] += 1;
                        continue;
                    };
                    match crate::path::find_path(&map, *pos, tile, |_| false) {
                        Some(path) => {
                            let item = crate::items::spawn_item(&mut commands, tile, kind);
                            // Machine output auto-enters the storage haul flow.
                            commands.entity(item).insert(MarkedForHaul);
                            commands.entity(item).insert(ReservedBy(crew_e));
                            local_claims.insert(item);
                            stats.haul_distance += crate::path::path_length(Some(*pos), &path);
                            set_haul_task(
                                &mut task,
                                &mut mov,
                                item,
                                HaulDest::Storage,
                                path,
                                crew_e,
                            );
                            let name = crew.name.clone();
                            log.push(
                                now,
                                LogKind::Info,
                                format!(
                                    "{} picked up {} from the fabricator output",
                                    name,
                                    kind.label()
                                ),
                            );
                            claimed = true;
                        }
                        None => {
                            f.output[kind.index()] += 1;
                        }
                    }
                }
                Cand::Build { bp } => {
                    let Ok((_, foot, bp_c, _)) = blueprints.get(bp) else {
                        continue;
                    };
                    let Some(path) = building::path_to_interaction(&map, *pos, foot) else {
                        continue;
                    };
                    let secs = building::def(bp_c.kind).work_secs;
                    commands.entity(bp).insert(ReservedBy(crew_e));
                    local_claims.insert(bp);
                    *task = CrewTask::Build(WorkJob {
                        target: bp,
                        phase: WorkPhase::Going,
                        timer: secs,
                        total: secs,
                    });
                    mov.path = path;
                    let name = crew.name.clone();
                    log.push(
                        now,
                        LogKind::Info,
                        format!("{name} started building {}", bp_c.kind.label()),
                    );
                    claimed = true;
                }
                Cand::Demo { building } => {
                    let Ok((_, foot, _, _)) = buildings.get(building) else {
                        continue;
                    };
                    let Some(path) = building::path_to_interaction(&map, *pos, foot) else {
                        continue;
                    };
                    commands.entity(building).insert(ReservedBy(crew_e));
                    local_claims.insert(building);
                    *task = CrewTask::Deconstruct(WorkJob {
                        target: building,
                        phase: WorkPhase::Going,
                        timer: 2.0,
                        total: 2.0,
                    });
                    mov.path = path;
                    let name = crew.name.clone();
                    log.push(now, LogKind::Info, format!("{name} started deconstruction"));
                    claimed = true;
                }
                Cand::Operate { fab } => {
                    let Ok((_, foot, _, _, _)) = fabs.get(fab) else {
                        continue;
                    };
                    let Some(path) = building::path_to_interaction(&map, *pos, foot) else {
                        continue;
                    };
                    commands.entity(fab).insert(ReservedBy(crew_e));
                    local_claims.insert(fab);
                    *task = CrewTask::Operate(WorkJob {
                        target: fab,
                        phase: WorkPhase::Going,
                        timer: crate::production::RECIPE.work_secs,
                        total: crate::production::RECIPE.work_secs,
                    });
                    mov.path = path;
                    let name = crew.name.clone();
                    log.push(
                        now,
                        LogKind::Info,
                        format!("{name} is operating the fabricator"),
                    );
                    claimed = true;
                }
            }
            if claimed {
                break;
            }
        }

        if !claimed {
            let cause = if marked_any_free && !storage_ok {
                IdleCause::NoStorageSpace
            } else if marked_any_free {
                IdleCause::AllUnreachable
            } else if marked_exists {
                IdleCause::AllClaimed
            } else {
                IdleCause::NothingToDo
            };
            *task = CrewTask::Idle(cause);
            crew.next_scan = now + SCAN_IDLE_SLOW as f64;
        }
    }
}

/// Can a hauler standing at `from` actually reach the destination's
/// interaction tiles? Validated at claim time so an unreachable consumer
/// (e.g. a blueprint sealed behind walls) never attracts pulls — otherwise
/// supply hauls would fetch from a rack, fail at pickup, convert to storage
/// and repeat forever (the "storage pump" loop observed in scenario F).
fn dest_reachable(
    map: &ShipMap,
    from: TilePos,
    dest: HaulDest,
    bp_foot: impl Fn(Entity) -> Option<Footprint>,
    machine_foot: impl Fn(Entity) -> Option<Footprint>,
) -> bool {
    match dest {
        HaulDest::Storage => true, // validated at pickup via choose_rack
        HaulDest::Blueprint(bp_e) => bp_foot(bp_e)
            .is_some_and(|foot| crate::building::path_to_interaction(map, from, &foot).is_some()),
        HaulDest::Machine(m) => machine_foot(m)
            .is_some_and(|foot| crate::building::path_to_interaction(map, from, &foot).is_some()),
    }
}

fn dest_entity(dest: HaulDest) -> Option<Entity> {
    match dest {
        HaulDest::Blueprint(e) | HaulDest::Machine(e) => Some(e),
        HaulDest::Storage => None,
    }
}

/// Assign a freshly claimed haul job (shared by every claim path).
fn set_haul_task(
    task: &mut CrewTask,
    mov: &mut Movement,
    item: Entity,
    dest: HaulDest,
    path: Vec<TilePos>,
    _crew: Entity,
) {
    *task = CrewTask::Haul(HaulJob {
        item,
        phase: HaulPhase::ToItem,
        dest,
        target_rack: None,
        timer: 0.0,
    });
    mov.path = path;
    mov.progress = 0.0;
}
