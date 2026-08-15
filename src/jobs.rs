//! The haul-job system: claiming, reservation, phase transitions and failure
//! recovery.
//!
//! There is no separate "job board" data structure. A haul job is derived
//! state: `MarkedForHaul` on an item is the player's intent, `ReservedBy` is
//! the claim, and the claiming crew's `CrewTask::Haul` holds the execution
//! state. This keeps a single source of truth per question ("is this item
//! wanted?", "who owns it?") and makes stale reservations impossible as long
//! as every code path that ends a job also releases the claim.

use crate::crew::{Crew, CrewTask, HaulJob, HaulPhase, IdleCause, Movement};
use crate::items::{CarriedBy, Item, ItemKind, MarkedForHaul, NoPathUntil, ReservedBy};
use crate::log::{EventLog, LogKind};
use crate::map::{find_drop_tile, ShipMap, TilePos};
use crate::storage::StorageCell;
use bevy::prelude::*;
use std::collections::HashSet;

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
    /// Set simulation speed by index into [`crate::SPEED_STEPS`].
    SetSpeed { index: usize },
}

pub struct JobsPlugin;

impl Plugin for JobsPlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<Action>();
        app.add_systems(
            Update,
            (actions_system, crew_task_system, crew_scan_system)
                .chain()
                .in_set(crate::Set::Jobs),
        );
    }
}

// =====================================================================================
// Player actions
// =====================================================================================

/// Handle player actions that mutate job/item state. Runs before job updates
/// so cancelled jobs settle within the same frame.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
pub fn actions_system(
    mut events: EventReader<Action>,
    mut commands: Commands,
    map: Res<ShipMap>,
    mut log: ResMut<EventLog>,
    time: Res<Time<Virtual>>,
    mut crews: Query<(Entity, &mut Crew, &mut CrewTask, &TilePos, &mut Movement), Without<Item>>,
    items: Query<(Entity, &TilePos, Option<&MarkedForHaul>), With<Item>>,
    rack_tiles: Query<&TilePos, (With<StorageCell>, Without<Crew>)>,
) {
    let now = time.elapsed().as_secs_f64();
    let racks: Vec<(TilePos, Entity)> = rack_tiles.iter().map(|p| (*p, Entity::PLACEHOLDER)).collect();

    for action in events.read() {
        match *action {
            Action::ToggleMark { item } => {
                if let Ok((_, _, marked)) = items.get(item) {
                    if marked.is_some() {
                        end_hauls_for_item(
                            &mut crews,
                            &mut commands,
                            &map,
                            &racks,
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
                    log.push(now, LogKind::Info, format!("Marked {n} items for hauling (box select)"));
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
                            &racks,
                            *pos,
                            &name,
                            item,
                            &mut log,
                            now,
                            IdleCause::JobCanceled { detail: "player canceled all jobs".into() },
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
                        &racks,
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
            Action::SetSpeed { .. } => {
                // Consumed by time_ctrl::speed_action_system.
            }
            Action::ToggleDebug => {
                // Consumed by ui::debug_toggle_system.
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
                    IdleCause::JobCanceled { detail: detail.clone() },
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
        CrewTask::Haul(j) => matches!(j.phase, HaulPhase::ToStorage | HaulPhase::Storing),
        CrewTask::Idle(_) => false,
    };
    if carrying {
        let drop = find_drop_tile(map, crew_pos, racks).unwrap_or(crew_pos);
        commands.entity(item).insert(TilePos::new(drop.x, drop.y));
        commands.entity(item).remove::<(CarriedBy, MarkedForHaul, ReservedBy)>();
        log.push(
            now,
            LogKind::Fail,
            format!("{crew_name} dropped the item at ({},{}): {}", drop.x, drop.y, cause.label()),
        );
    } else {
        commands.entity(item).remove::<(ReservedBy,)>();
        log.push(now, LogKind::Fail, format!("{crew_name}: {}", cause.label()));
    }
    *task = CrewTask::Idle(cause);
    mov.path.clear();
    mov.progress = 0.0;
}

// =====================================================================================
// Job execution
// =====================================================================================

fn racks_with_space(racks: &Query<(Entity, &TilePos, &mut StorageCell), Without<Crew>>) -> Vec<(Entity, TilePos)> {
    racks
        .iter()
        .filter(|(_, _, s)| s.has_space())
        .map(|(e, p, _)| (e, *p))
        .collect()
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
    let rack_list: Vec<(TilePos, Entity)> = racks
        .iter()
        .map(|(e, p, _)| (*p, e))
        .collect();
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
        IdleCause::JobFailed { detail: detail.to_string() },
    );
    crew.next_scan = now + 0.3;
}

/// Advance every active haul job through its phases.
#[allow(clippy::type_complexity)]
pub fn crew_task_system(
    map: Res<ShipMap>,
    time: Res<Time<Virtual>>,
    mut log: ResMut<EventLog>,
    mut commands: Commands,
    mut crews: Query<(Entity, &mut Crew, &mut CrewTask, &mut TilePos, &mut Movement), Without<Item>>,
    items: Query<(Entity, &TilePos, &Item, Option<&MarkedForHaul>, Option<&CarriedBy>), With<Item>>,
    mut racks: Query<(Entity, &TilePos, &mut StorageCell), Without<Crew>>,
) {
    let dt = time.delta().as_secs_f32();
    let now = time.elapsed().as_secs_f64();

    for (crew_e, mut crew, mut task, pos, mut mov) in crews.iter_mut() {
        let CrewTask::Haul(job) = &mut *task else {
            continue;
        };
        let item_entity = job.item;

        // ---- validate target -------------------------------------------------
        let Ok((_, item_pos, item, marked, carried)) = items.get(item_entity) else {
            let name = crew.name.clone();
            *task = CrewTask::Idle(IdleCause::JobCanceled { detail: "target vanished".into() });
            mov.path.clear();
            crew.next_scan = now + 0.2;
            log.push(now, LogKind::Fail, format!("{name}: job canceled — target vanished"));
            continue;
        };
        if marked.is_none() {
            let name = crew.name.clone();
            commands.entity(item_entity).remove::<ReservedBy>();
            *task = CrewTask::Idle(IdleCause::JobCanceled { detail: "item unmarked".into() });
            mov.path.clear();
            crew.next_scan = now + 0.2;
            log.push(now, LogKind::Fail, format!("{name}: job canceled — item unmarked"));
            continue;
        }
        if let Some(by) = carried {
            if by.0 != crew_e {
                let name = crew.name.clone();
                commands.entity(item_entity).remove::<ReservedBy>();
                *task = CrewTask::Idle(IdleCause::JobCanceled { detail: "item claimed elsewhere".into() });
                mov.path.clear();
                crew.next_scan = now + 0.2;
                log.push(now, LogKind::Fail, format!("{name}: job canceled — item claimed elsewhere"));
                continue;
            }
        }

        match job.phase {
            HaulPhase::ToItem => {
                if mov.path.is_empty() && *pos == *item_pos {
                    job.phase = HaulPhase::PickingUp;
                    job.timer = 0.3;
                } else if mov.path.is_empty() {
                    match crate::path::find_path(&map, *pos, *item_pos, |_| false) {
                        Some(p) => mov.path = p,
                        None => fail_haul(&mut commands, &map, &racks, &mut task, &mut mov, &mut crew, *pos, item_entity, &mut log, now, "path to item lost"),
                    }
                }
            }
            HaulPhase::PickingUp => {
                job.timer -= dt;
                if job.timer <= 0.0 {
                    if *pos == *item_pos {
                        commands.entity(item_entity).insert(CarriedBy(crew_e));
                        match choose_rack(&map, *pos, &racks_with_space(&racks)) {
                            Some((rack, path)) => {
                                job.target_rack = Some(rack);
                                job.phase = HaulPhase::ToStorage;
                                mov.path = path;
                            }
                            None => {
                                // The item is in the crew's hands now (CarriedBy
                                // above): make sure the failure path drops it
                                // instead of leaving it phantom-carried.
                                job.phase = HaulPhase::ToStorage;
                                fail_haul(&mut commands, &map, &racks, &mut task, &mut mov, &mut crew, *pos, item_entity, &mut log, now, "no reachable storage with space");
                            }
                        }
                    } else {
                        // Item ended up elsewhere: walk to it again.
                        job.phase = HaulPhase::ToItem;
                    }
                }
            }
            HaulPhase::ToStorage => {
                let Some(rack_e) = job.target_rack else {
                    // No rack chosen yet (came from a failed retry): pick one.
                    match choose_rack(&map, *pos, &racks_with_space(&racks)) {
                        Some((rack, path)) => {
                            job.target_rack = Some(rack);
                            mov.path = path;
                        }
                        None => fail_haul(&mut commands, &map, &racks, &mut task, &mut mov, &mut crew, *pos, item_entity, &mut log, now, "no reachable storage with space"),
                    }
                    continue;
                };
                let Ok((_, rack_pos, rack)) = racks.get(rack_e) else {
                    match choose_rack(&map, *pos, &racks_with_space(&racks)) {
                        Some((rack, path)) => {
                            job.target_rack = Some(rack);
                            mov.path = path;
                        }
                        None => fail_haul(&mut commands, &map, &racks, &mut task, &mut mov, &mut crew, *pos, item_entity, &mut log, now, "storage disappeared"),
                    }
                    continue;
                };
                if mov.path.is_empty() && *pos == *rack_pos {
                    if rack.has_space() {
                        job.phase = HaulPhase::Storing;
                        job.timer = 0.25;
                    } else {
                        // Rack filled up en route: switch to another one.
                        match choose_rack(&map, *pos, &racks_with_space(&racks)) {
                            Some((rack, path)) => {
                                job.target_rack = Some(rack);
                                mov.path = path;
                            }
                            None => fail_haul(&mut commands, &map, &racks, &mut task, &mut mov, &mut crew, *pos, item_entity, &mut log, now, "no free storage space"),
                        }
                    }
                } else if mov.path.is_empty() {
                    match crate::path::find_path(&map, *pos, *rack_pos, |_| false) {
                        Some(p) => mov.path = p,
                        None => fail_haul(&mut commands, &map, &racks, &mut task, &mut mov, &mut crew, *pos, item_entity, &mut log, now, "path to storage lost"),
                    }
                }
            }
            HaulPhase::Storing => {
                job.timer -= dt;
                if job.timer <= 0.0 {
                    let Some(rack_e) = job.target_rack else {
                        job.phase = HaulPhase::ToStorage;
                        continue;
                    };
                    let Ok((_, rack_pos, mut rack_cell)) = racks.get_mut(rack_e) else {
                        fail_haul(&mut commands, &map, &racks, &mut task, &mut mov, &mut crew, *pos, item_entity, &mut log, now, "storage disappeared");
                        continue;
                    };
                    if *pos == *rack_pos && rack_cell.has_space() {
                        rack_cell.try_add(item.kind);
                        crew.delivered += 1;
                        let name = crew.name.clone();
                        let kind = item.kind.label();
                        log.push(now, LogKind::Job, format!("{name} stored {kind}"));
                        commands.entity(item_entity).despawn();
                        *task = CrewTask::Idle(IdleCause::Looking);
                        crew.next_scan = now; // immediately look for the next job
                    } else {
                        // Filled between arrival and now: pick another rack.
                        job.phase = HaulPhase::ToStorage;
                    }
                }
            }
        }
    }
}

/// Nearest reachable rack that still has space: (rack, path).
fn choose_rack(map: &ShipMap, from: TilePos, racks: &[(Entity, TilePos)]) -> Option<(Entity, Vec<TilePos>)> {
    let mut sorted = racks.to_vec();
    sorted.sort_by_key(|(_, p)| (from.x - p.x).abs() + (from.y - p.y).abs());
    for (e, p) in sorted {
        if let Some(path) = crate::path::find_path(map, from, p, |_| false) {
            return Some((e, path));
        }
    }
    None
}

// =====================================================================================
// Job claiming
// =====================================================================================

/// Idle crew look for the nearest claimable, reachable, marked item.
/// Storage must have at least one rack with free space (checked without
/// pathing; per-rack reachability is validated when a delivery route is chosen).
#[allow(clippy::type_complexity)]
pub fn crew_scan_system(
    map: Res<ShipMap>,
    time: Res<Time<Virtual>>,
    mut log: ResMut<EventLog>,
    mut commands: Commands,
    mut crews: Query<(Entity, &mut Crew, &mut CrewTask, &TilePos, &mut Movement)>,
    items: Query<
        (
            Entity,
            &TilePos,
            &Item,
            Option<&ReservedBy>,
            Option<&CarriedBy>,
            Option<&NoPathUntil>,
        ),
        With<MarkedForHaul>,
    >,
    racks: Query<&StorageCell>,
) {
    let now = time.elapsed().as_secs_f64();

    // Claiming happens through Commands, which flush after this system; keep a
    // local set so two idle crews in the same frame never claim the same item.
    let mut local_claims: HashSet<Entity> = HashSet::new();

    for (crew_e, mut crew, mut task, pos, mut mov) in crews.iter_mut() {
        if !matches!(*task, CrewTask::Idle(_)) || now < crew.next_scan {
            continue;
        }

        let marked: Vec<_> = items.iter().map(|(e, p, it, r, c, n)| (e, *p, it.kind, r.is_some(), c.is_some(), n)).collect();
        if marked.is_empty() {
            *task = CrewTask::Idle(IdleCause::NoMarkedItems);
            crew.next_scan = now + 0.6;
            continue;
        }

        if !racks.iter().any(|s| s.has_space()) {
            *task = CrewTask::Idle(IdleCause::NoStorageSpace);
            crew.next_scan = now + 1.0;
            continue;
        }

        // Claimable now: marked, unreserved, not carried, not on cooldown.
        let mut candidates: Vec<_> = marked
            .iter()
            .filter(|(e, _, _, reserved, carried, _)| {
                !*reserved && !*carried && !local_claims.contains(e)
            })
            .filter(|(_, _, _, _, _, cooled)| cooled.map(|c| c.0 <= now).unwrap_or(true))
            .map(|(e, p, kind, ..)| (*e, *p, *kind))
            .collect();
        if candidates.is_empty() {
            let any_free = marked.iter().any(|(e, _, _, reserved, carried, _)| {
                !*reserved && !*carried && !local_claims.contains(e)
            });
            let cause = if any_free {
                IdleCause::AllUnreachable // only cooldowns filtered them out
            } else {
                IdleCause::AllClaimed
            };
            *task = CrewTask::Idle(cause);
            crew.next_scan = now + 0.6;
            continue;
        }

        // Try nearest-first; the first reachable one is claimed.
        candidates.sort_by_key(|(_, p, _)| (pos.x - p.x).abs() + (pos.y - p.y).abs());
        let mut claimed = false;
        for (item_e, item_pos, kind) in candidates {
            match crate::path::find_path(&map, *pos, item_pos, |_| false) {
                Some(path) => {
                    let name = crew.name.clone();
                    commands.entity(item_e).insert(ReservedBy(crew_e));
                    local_claims.insert(item_e);
                    *task = CrewTask::Haul(HaulJob {
                        item: item_e,
                        phase: HaulPhase::ToItem,
                        target_rack: None,
                        repaths: 0,
                        timer: 0.0,
                    });
                    mov.path = path;
                    log.push(
                        now,
                        LogKind::Info,
                        format!("{name} claimed {} at ({},{})", kind.label(), item_pos.x, item_pos.y),
                    );
                    claimed = true;
                    break;
                }
                None => {
                    commands.entity(item_e).insert(NoPathUntil(now + EventLog::UNREACHABLE_COOLDOWN));
                    log.push(
                        now,
                        LogKind::Fail,
                        format!("{} at ({},{}) is unreachable", kind.label(), item_pos.x, item_pos.y),
                    );
                }
            }
        }
        if !claimed {
            *task = CrewTask::Idle(IdleCause::AllUnreachable);
            crew.next_scan = now + 1.0;
        }
    }
}
