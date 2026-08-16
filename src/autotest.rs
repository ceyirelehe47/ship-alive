//! Automated acceptance-scenario driver (dev tool).
//!
//! `SLICE0_SCENARIO=A..L cargo run` scripts player actions on a schedule
//! (mark items, place blueprints, set orders and priorities, …), then prints
//! a world state summary and exits. Used to smoke-test the acceptance
//! scenarios from the design briefs without manual play.

use crate::building::{Blueprint, Building, BuildingKind, Footprint};
use crate::crew::{Crew, CrewTask, Priority, WorkKind};
use crate::items::{Item, ItemKind, MarkedForHaul, ReservedBy};
use crate::jobs::Action;
use crate::log::EventLog;
use crate::map::TilePos;
use crate::production::Fabricator;
use crate::storage::StorageCell;
use bevy::prelude::*;

pub struct AutotestPlugin;

impl Plugin for AutotestPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                scenario_driver,
                slice2_driver,
                slice3_driver,
                slice4_driver,
                slice5_driver,
                slice5_dev_tools,
                slice4_dev_pins,
            )
                .in_set(crate::Set::Input),
        );
    }
}

fn fire(
    tag: &'static str,
    at: f64,
    t: f64,
    fired: &mut Vec<&'static str>,
    actions: &mut EventWriter<Action>,
    f: impl FnOnce(&mut EventWriter<Action>),
) {
    if t >= at && !fired.contains(&tag) {
        fired.push(tag);
        f(actions);
    }
}

#[allow(clippy::too_many_arguments)]
fn dump_and_exit(
    ctx: &str,
    items: &Query<(Entity, &Item), With<MarkedForHaul>>,
    crews: &Query<(Entity, &Crew, &CrewTask), With<Crew>>,
    racks: &Query<(Entity, &TilePos, &StorageCell)>,
    reserved: &Query<(Entity, &ReservedBy)>,
    stats: &crate::stats::Stats,
    log: &EventLog,
    exit: &mut EventWriter<AppExit>,
) {
    let stored: u32 = racks.iter().map(|(_, _, s)| s.stored()).sum();
    let free: u32 = racks.iter().map(|(_, _, s)| s.free()).sum();
    let crew: Vec<String> = crews
        .iter()
        .map(|(_, c, t)| {
            let task = match t {
                CrewTask::Idle(c) => c.label(),
                CrewTask::Haul(_) => "hauling".to_string(),
                CrewTask::Build(_) => "building".to_string(),
                CrewTask::Deconstruct(_) => "deconstructing".to_string(),
                CrewTask::Operate(_) => "operating".to_string(),
            };
            format!(
                "{}[h={},b={},o={}]: {}",
                c.name, c.delivered, c.built, c.operated, task
            )
        })
        .collect();
    println!(
        "SCENARIO_RESULT scenario={ctx} marked_left={} stored={stored} free={free} reserved={} stats=[{}] crew={crew:?}",
        items.iter().count(),
        reserved.iter().count(),
        stats.summary(),
    );
    println!("LOG_TAIL_BEGIN");
    for e in log
        .entries
        .iter()
        .rev()
        .take(14)
        .collect::<Vec<_>>()
        .iter()
        .rev()
    {
        println!("  [{:.1}s] {:?} {}", e.time, e.kind, e.text);
    }
    println!("LOG_TAIL_END");
    exit.write(AppExit::Success);
}

#[allow(clippy::too_many_arguments)]
fn scenario_driver(
    clock: Res<crate::simtime::SimClock>,
    mut actions: EventWriter<Action>,
    items: Query<(Entity, &Item), With<MarkedForHaul>>,
    reserved: Query<(Entity, &ReservedBy)>,
    crews: Query<(Entity, &Crew, &CrewTask), With<Crew>>,
    racks: Query<(Entity, &TilePos, &StorageCell)>,
    buildings: Query<(Entity, &TilePos, &Building)>,
    bps_q: Query<(Entity, &TilePos, &Blueprint)>,
    fabs: Query<(Entity, &TilePos, &Fabricator)>,
    stats: Res<crate::stats::Stats>,
    log: Res<EventLog>,
    mut exit: EventWriter<AppExit>,
    mut fired: Local<Vec<&'static str>>,
    mut last_trace: Local<f64>,
    mut rework: Local<(f32, u32)>,
    trace_crews: Query<(
        &Crew,
        &CrewTask,
        &crate::map::TilePos,
        &crate::crew::Movement,
    )>,
) {
    let Some(scenario) = std::env::var("SLICE0_SCENARIO").ok() else {
        return;
    };
    // Old-gameplay-second semantics (1 unit = 1 real s at 1×) so the
    // historically tuned scenario thresholds keep their meaning.
    let t = clock.now() / crate::simtime::BASE_SIM_RATE;

    // Optional movement trace for debugging congestion (SLICE0_TRACE=1).
    if std::env::var("SLICE0_TRACE").is_ok() && t - *last_trace >= 2.0 {
        *last_trace = t;
        for (crew, task, pos, mov) in trace_crews.iter() {
            let phase = match task {
                CrewTask::Idle(c) => c.label(),
                CrewTask::Haul(j) => format!("{:?}", j.phase),
                CrewTask::Build(_) | CrewTask::Deconstruct(_) | CrewTask::Operate(_) => {
                    "working".to_string()
                }
            };
            println!(
                "TRACE t={t:.1} {} pos=({},{}) path={} prog={:.2} blocked={:.2} pass={} task={phase}",
                crew.name, pos.x, pos.y, mov.path.len(), mov.progress, mov.blocked_for, mov.passing_through
            );
        }
    }

    match scenario.as_str() {
        // A: normal hauling — mark everything, fast-forward, expect all stored.
        "A" => {
            fire("mark", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            fire("speed", 0.6, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"done") && t >= 150.0 || (t >= 20.0 && items.iter().count() == 0) {
                fired.push("done");
                dump_and_exit(
                    "A", &items, &crews, &racks, &reserved, &stats, &log, &mut exit,
                );
            }
        }
        // B: storage full — spawn items beyond capacity, expect idle + no thrash.
        "B" => {
            fire("mark", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            fire("spawn", 0.5, t, &mut fired, &mut actions, |a| {
                for _ in 0..14 {
                    let _ = a.write(Action::SpawnItem {
                        kind: ItemKind::Crate,
                    });
                }
            });
            fire("speed", 0.7, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"done") && t >= 160.0 {
                fired.push("done");
                dump_and_exit(
                    "B", &items, &crews, &racks, &reserved, &stats, &log, &mut exit,
                );
            }
        }
        // C: unreachable target — the sealed pocket item is marked with the rest.
        "C" => {
            fire("mark", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            if !fired.contains(&"done") && t >= 15.0 {
                fired.push("done");
                dump_and_exit(
                    "C", &items, &crews, &racks, &reserved, &stats, &log, &mut exit,
                );
            }
        }
        // D: competition — all four crew wake at once against few items.
        "D" => {
            fire("mark", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            if !fired.contains(&"done") && t >= 5.0 {
                fired.push("done");
                dump_and_exit(
                    "D", &items, &crews, &racks, &reserved, &stats, &log, &mut exit,
                );
            }
        }
        // E: invalidation — delete a claimed target mid-route.
        "E" => {
            fire("mark", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            if t >= 4.0 && !fired.contains(&"delete") {
                if let Some((e, _)) = reserved.iter().next() {
                    fired.push("delete");
                    let _ = actions.write(Action::DeleteItem { item: e });
                }
            }
            if !fired.contains(&"done") && t >= 14.0 {
                fired.push("done");
                dump_and_exit(
                    "E", &items, &crews, &racks, &reserved, &stats, &log, &mut exit,
                );
            }
        }
        // F: max speed under load — everything marked at 4x plus spawned items.
        "F" => {
            fire("mark", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            fire("spawn", 0.5, t, &mut fired, &mut actions, |a| {
                for _ in 0..10 {
                    let _ = a.write(Action::SpawnItem {
                        kind: ItemKind::Ore,
                    });
                }
            });
            fire("speed", 0.8, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"done") && t >= 150.0 || (t >= 30.0 && items.iter().count() == 0) {
                fired.push("done");
                dump_and_exit(
                    "F", &items, &crews, &racks, &reserved, &stats, &log, &mut exit,
                );
            }
        }
        // G: build — place a rack blueprint; materials get hauled and built.
        "G" => {
            fire("place", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Rack,
                    pos: TilePos::new(13, 11),
                });
            });
            fire("speed", 0.6, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            let built = buildings
                .iter()
                .any(|(_, p, b)| b.kind == BuildingKind::Rack && p.x == 13 && p.y == 11);
            if !fired.contains(&"done") && (t >= 20.0 && built || t >= 120.0) {
                fired.push("done");
                println!(
                    "G_BUILD_RESULT built_at_target={built} buildings_total={}",
                    buildings.iter().count()
                );
                dump_and_exit(
                    "G", &items, &crews, &racks, &reserved, &stats, &log, &mut exit,
                );
            }
        }
        // H: deconstruct — tear down a starter rack, expect refund + cleanup.
        "H" => {
            if t >= 0.3 && !fired.contains(&"mark") {
                if let Some((e, _, _)) = racks.iter().find(|(_, p, _)| p.x == 29 && p.y == 10) {
                    fired.push("mark");
                    let _ = actions.write(Action::MarkDeconstruct { building: e });
                }
            }
            fire("speed", 0.6, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"done") && (t >= 10.0 && stats.deconstructed >= 1 || t >= 120.0) {
                fired.push("done");
                println!(
                    "H_DEMO_RESULT deconstructed={} racks_left={}",
                    stats.deconstructed,
                    racks.iter().count()
                );
                dump_and_exit(
                    "H", &items, &crews, &racks, &reserved, &stats, &log, &mut exit,
                );
            }
        }
        // I: production — order parts, expect ore in, worker, parts out, stored.
        "I" => {
            if t >= 0.3 && !fired.contains(&"order") {
                if let Some((e, _, _)) = fabs.iter().next() {
                    fired.push("order");
                    let _ = actions.write(Action::FabAddOrder { fab: e, batches: 5 });
                }
            }
            fire("speed", 0.6, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"done") && (t >= 30.0 && stats.produced >= 3 || t >= 240.0) {
                fired.push("done");
                println!("I_PROD_RESULT produced={}", stats.produced);
                dump_and_exit(
                    "I", &items, &crews, &racks, &reserved, &stats, &log, &mut exit,
                );
            }
        }
        // J: storage filters — dedicate racks to ore / parts and check routing.
        "J" => {
            if t >= 0.3 && !fired.contains(&"filter") {
                let mut ore_rack: Option<Entity> = None;
                let mut part_rack: Option<Entity> = None;
                for (e, p, _) in racks.iter() {
                    if p.x == 29 && p.y == 10 {
                        ore_rack = Some(e);
                    }
                    if p.x == 29 && p.y == 11 {
                        part_rack = Some(e);
                    }
                }
                if let (Some(o), Some(p)) = (ore_rack, part_rack) {
                    fired.push("filter");
                    for kind in ItemKind::ALL {
                        let ore_ok = kind == ItemKind::Ore;
                        let part_ok = kind == ItemKind::Part;
                        let _ = actions.write(Action::SetRackFilter {
                            rack: o,
                            kind,
                            allowed: ore_ok,
                        });
                        let _ = actions.write(Action::SetRackFilter {
                            rack: p,
                            kind,
                            allowed: part_ok,
                        });
                    }
                }
            }
            fire("mark", 0.5, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            fire("speed", 0.7, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"done") && t >= 100.0 {
                fired.push("done");
                for (_, p, cell) in racks.iter() {
                    if p.x == 29 && (p.y == 10 || p.y == 11) {
                        println!(
                            "J_RACK pos=({},{}) counts={:?} allowed={:?}",
                            p.x, p.y, cell.counts, cell.allowed
                        );
                    }
                }
                dump_and_exit(
                    "J", &items, &crews, &racks, &reserved, &stats, &log, &mut exit,
                );
            }
        }
        // K: job competition — dedicate crew to haul / build / operate.
        "K" => {
            if t >= 0.2 && !fired.contains(&"prio") {
                fired.push("prio");
                let roster: Vec<Entity> = crews.iter().map(|(e, _, _)| e).collect();
                // crew[0]: haul only; crew[1]: build only; crew[2]: operate only; crew[3]: all.
                for (i, e) in roster.iter().enumerate() {
                    for wk in WorkKind::ALL {
                        let level = match (i, wk) {
                            (0, WorkKind::Haul) | (1, WorkKind::Build) | (2, WorkKind::Operate) => {
                                Priority::High
                            }
                            (0, WorkKind::Build) | (0, WorkKind::Operate) => Priority::Disabled,
                            (1, WorkKind::Haul) | (1, WorkKind::Operate) => Priority::Disabled,
                            (2, WorkKind::Haul) | (2, WorkKind::Build) => Priority::Disabled,
                            _ => Priority::Normal,
                        };
                        let _ = actions.write(Action::SetPriority {
                            crew: *e,
                            work: wk,
                            level,
                        });
                    }
                }
            }
            if t >= 0.4 && !fired.contains(&"work") {
                fired.push("work");
                if let Some((e, _, _)) = fabs.iter().next() {
                    let _ = actions.write(Action::FabAddOrder { fab: e, batches: 2 });
                }
                let _ = actions.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Wall,
                    pos: TilePos::new(13, 11),
                });
                // Ore comes from rack stock via auto-logistics pulls.
            }
            fire("speed", 0.6, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"done") && (t >= 240.0 || (t >= 60.0 && stats.produced >= 2)) {
                fired.push("done");
                for (e, _, f) in fabs.iter() {
                    println!(
                        "K_FAB state={:?} in={:?} out={:?} active={}",
                        f.state(),
                        f.input,
                        f.output,
                        f.active
                    );
                    let _ = e;
                }
                println!("K_SPLIT_RESULT (see crew counters in SCENARIO_RESULT)");
                dump_and_exit(
                    "K", &items, &crews, &racks, &reserved, &stats, &log, &mut exit,
                );
            }
        }
        // L: 4x stress — many blueprints, orders and marked items at once.
        "L" => {
            if t >= 0.3 && !fired.contains(&"load") {
                fired.push("load");
                let _ = actions.write(Action::MarkAll);
                let _ = actions.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Wall,
                    pos: TilePos::new(13, 12),
                });
                let _ = actions.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Rack,
                    pos: TilePos::new(19, 11),
                });
                let _ = actions.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Door,
                    pos: TilePos::new(27, 9),
                });
                if let Some((e, _, _)) = fabs.iter().next() {
                    let _ = actions.write(Action::FabAddOrder { fab: e, batches: 3 });
                }
            }
            fire("spawn", 0.5, t, &mut fired, &mut actions, |a| {
                for _ in 0..6 {
                    let _ = a.write(Action::SpawnItem {
                        kind: ItemKind::Ore,
                    });
                }
            });
            fire("speed", 0.7, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"done") && t >= 180.0 {
                fired.push("done");
                let stuck = crews
                    .iter()
                    .filter(|(_, _, task)| match task {
                        CrewTask::Idle(c) => !matches!(
                            c,
                            crate::crew::IdleCause::NothingToDo
                                | crate::crew::IdleCause::Looking
                                | crate::crew::IdleCause::AllClaimed
                        ),
                        _ => false,
                    })
                    .count();
                println!(
                    "L_STRESS_RESULT built={} produced={} stuck_idle={stuck} reserved={}",
                    stats.built,
                    stats.produced,
                    reserved.iter().count()
                );
                dump_and_exit(
                    "L", &items, &crews, &racks, &reserved, &stats, &log, &mut exit,
                );
            }
        }
        // P1: playtest 1 - build flows (build rack/wall/door, deconstruct, rebuild).
        "P1" => {
            fire("p1_speed", 0.2, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            fire("p1_rack", 0.5, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Rack,
                    pos: TilePos::new(17, 11),
                });
            });
            fire("p1_wall", 0.7, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Wall,
                    pos: TilePos::new(13, 11),
                });
            });
            fire("p1_door", 0.9, t, &mut fired, &mut actions, |a| {
                // (27,9) is STORAGE's one-tile corridor opening; doors must
                // sit in a wall gap since Slice 4.
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Door,
                    pos: TilePos::new(27, 9),
                });
            });
            // Cancel the wall blueprint while its material may still be in transit.
            if t >= 3.0 && !fired.contains(&"p1_cancel") {
                if let Some((e, _, _)) = bps_q.iter().find(|(_, _, b)| b.kind == BuildingKind::Wall)
                {
                    fired.push("p1_cancel");
                    let _ = actions.write(Action::CancelBlueprint { blueprint: e });
                }
            }
            // Deconstruct the rack once it exists, then rebuild it.
            let rack_built = buildings
                .iter()
                .any(|(_, p, b)| b.kind == BuildingKind::Rack && p.x == 17 && p.y == 11);
            if rack_built && !fired.contains(&"p1_demo") {
                fired.push("p1_demo");
                if let Some((e, _, _)) = buildings
                    .iter()
                    .find(|(_, p, b)| b.kind == BuildingKind::Rack && p.x == 17 && p.y == 11)
                {
                    let _ = actions.write(Action::MarkDeconstruct { building: e });
                }
            }
            let rack_gone = !buildings
                .iter()
                .any(|(_, p, b)| b.kind == BuildingKind::Rack && p.x == 17 && p.y == 11);
            if rack_gone && !fired.contains(&"p1_rebuild") && fired.contains(&"p1_demo") {
                fired.push("p1_rebuild");
                let _ = actions.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Rack,
                    pos: TilePos::new(17, 11),
                });
            }
            let rebuilt = buildings
                .iter()
                .any(|(_, p, b)| b.kind == BuildingKind::Rack && p.x == 17 && p.y == 11)
                && fired.contains(&"p1_rebuild");
            let door_built = buildings
                .iter()
                .any(|(_, p, b)| b.kind == BuildingKind::Door && p.x == 27 && p.y == 9);
            if !fired.contains(&"done") && ((t >= 20.0 && rebuilt && door_built) || t >= 240.0) {
                fired.push("done");
                println!(
                    "P1_RESULT rack_rebuilt={rebuilt} wall_canceled={} door_built={}",
                    fired.contains(&"p1_cancel"),
                    buildings
                        .iter()
                        .any(|(_, p, b)| b.kind == BuildingKind::Door && p.x == 27 && p.y == 9),
                );
                dump_and_exit(
                    "P1", &items, &crews, &racks, &reserved, &stats, &log, &mut exit,
                );
            }
        }
        // P2: playtest 2 - production configuration (raw rack, parts rack, order, watch).
        "P2" => {
            if t >= 0.3 && !fired.contains(&"p2_filters") {
                fired.push("p2_filters");
                for (e, p, _) in racks.iter() {
                    // P racks at (28,10),(28,11): parts only. O racks (28,13),(28,14): ore only.
                    let (allow_ore, allow_part) = if p.y <= 11 {
                        (false, true)
                    } else {
                        (true, false)
                    };
                    for kind in ItemKind::ALL {
                        let allowed = match kind {
                            ItemKind::Ore => allow_ore,
                            ItemKind::Part => allow_part,
                            ItemKind::Crate => false,
                        };
                        let _ = actions.write(Action::SetRackFilter {
                            rack: e,
                            kind,
                            allowed,
                        });
                    }
                }
            }
            if t >= 0.6 && !fired.contains(&"p2_ordr") {
                fired.push("p2_ordr");
                if let Some((e, _, _)) = fabs.iter().next() {
                    let _ = actions.write(Action::FabAddOrder { fab: e, batches: 3 });
                }
            }
            fire("speed", 0.8, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"done") && ((t >= 40.0 && stats.produced >= 2) || t >= 240.0) {
                fired.push("done");
                if let Some((_, _, f)) = fabs.iter().next() {
                    println!(
                        "P2_FAB state={:?} in={:?} out={:?}",
                        f.state(),
                        f.input,
                        f.output
                    );
                }
                let mut ore_in_parts_racks = 0;
                let mut parts_in_ore_racks = 0;
                for (_, p, c) in racks.iter() {
                    if p.y <= 11 {
                        ore_in_parts_racks += c.counts[ItemKind::Ore.index()];
                    } else {
                        parts_in_ore_racks += c.counts[ItemKind::Part.index()];
                    }
                }
                println!("P2_FILTER_CLEAN ore_in_parts_racks={ore_in_parts_racks} parts_in_ore_racks={parts_in_ore_racks}");
                dump_and_exit(
                    "P2", &items, &crews, &racks, &reserved, &stats, &log, &mut exit,
                );
            }
        }
        // M: playtest 3 - layout optimization A/B (far ore racks vs near-fab ore racks).
        "M" => {
            fire("m_speed", 0.2, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            // Phase A: baseline - repeat production with the stock (far) ore racks.
            if t >= 0.4 && !fired.contains(&"m_order_a") {
                fired.push("m_order_a");
                if let Some((e, _, _)) = fabs.iter().next() {
                    let _ = actions.write(Action::FabRepeat { fab: e });
                }
                println!("M_PHASE A start t={t:.1}");
            }
            if !fired.contains(&"m_a_done") && stats.produced >= 5 {
                fired.push("m_a_done");
                println!(
                    "M_BASELINE t_5_parts={t:.1} haul_dist={} hauls={}",
                    stats.haul_distance, stats.hauls_done
                );
            }
            // Phase B: rework - build two ore racks next to the fabricator,
            // deny ore everywhere else, tear down the far ore racks and
            // re-store the refunded ore into the new racks.
            if fired.contains(&"m_a_done") && !fired.contains(&"m_build") {
                fired.push("m_build");
                let _ = actions.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Rack,
                    pos: TilePos::new(17, 12),
                });
                let _ = actions.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Rack,
                    pos: TilePos::new(18, 12),
                });
            }
            let new_rocks: Vec<Entity> = buildings
                .iter()
                .filter(|(_, p, b)| {
                    b.kind == BuildingKind::Rack && p.y == 12 && (p.x == 17 || p.x == 18)
                })
                .map(|(e, _, _)| e)
                .collect();
            if new_rocks.len() == 2 && !fired.contains(&"m_filter") {
                fired.push("m_filter");
                for e in &new_rocks {
                    for kind in ItemKind::ALL {
                        let _ = actions.write(Action::SetRackFilter {
                            rack: *e,
                            kind,
                            allowed: kind == ItemKind::Ore,
                        });
                    }
                }
                // Deny ore on every other rack so re-stored ore must reach the new ones.
                for (e, p, _) in racks.iter() {
                    if p.y == 12 && (p.x == 17 || p.x == 18) {
                        continue;
                    }
                    let _ = actions.write(Action::SetRackFilter {
                        rack: e,
                        kind: ItemKind::Ore,
                        allowed: false,
                    });
                }
                println!("M_PHASE B racks built+filtered t={t:.1}");
            }
            let filtered = new_rocks.len() == 2 && fired.contains(&"m_filter");
            if filtered && !fired.contains(&"m_demo") {
                fired.push("m_demo");
                for (e, p, _) in racks.iter() {
                    if p.x == 28 && (p.y == 13 || p.y == 14) {
                        let _ = actions.write(Action::MarkDeconstruct { building: e });
                    }
                }
            }
            let old_gone = !racks
                .iter()
                .any(|(_, p, _)| p.x == 28 && (p.y == 13 || p.y == 14));
            if old_gone && fired.contains(&"m_demo") && !fired.contains(&"m_rework") {
                fired.push("m_rework");
                *rework = (stats.haul_distance, stats.hauls_done);
                // Box-select just the refund zone near the old racks and the
                // ore-bay ground ore (like a player would) so only ore enters
                // the re-storage flow.
                let _ = actions.write(Action::MarkArea {
                    from: Vec2::new(26.0 * 32.0, -17.0 * 32.0),
                    to: Vec2::new(32.0 * 32.0, -12.0 * 32.0),
                });
                let _ = actions.write(Action::MarkArea {
                    from: Vec2::new(23.0 * 32.0, -6.0 * 32.0),
                    to: Vec2::new(35.0 * 32.0, -32.0),
                });
                // Fresh ore (player keeps mining): stored into the only
                // accepting racks — the new ones next to the fabricator.
                for _ in 0..8 {
                    let _ = actions.write(Action::SpawnItem {
                        kind: ItemKind::Ore,
                    });
                }
                println!(
                    "M_PHASE B rework complete t={t:.1} (baseline haul_dist={})",
                    stats.haul_distance
                );
            }
            let reworked = fired.contains(&"m_rework");
            let produced_after = stats.produced;
            if reworked && !fired.contains(&"m_done") && produced_after >= 10 {
                fired.push("m_done");
                // Deltas since the rework completed.
                println!(
                    "M_IMPROVED t_next_5_parts={:.1} haul_dist_since_rework={:.0} hauls_since_rework={}",
                    t,
                    (stats.haul_distance - rework.0).max(0.0),
                    stats.hauls_done.saturating_sub(rework.1),
                );
                dump_and_exit(
                    "M", &items, &crews, &racks, &reserved, &stats, &log, &mut exit,
                );
            }
            if !fired.contains(&"m_done") && t >= 600.0 {
                fired.push("m_done");
                println!("M_TIMEOUT produced={produced_after}");
                dump_and_exit(
                    "M", &items, &crews, &racks, &reserved, &stats, &log, &mut exit,
                );
            }
        }
        _ => {}
    }
}

// =====================================================================================
// Slice 2 acceptance scenarios (Ship Power), driven by SLICE2_SCENARIO=A..J.
// Coordinates refer to the starter ship: reactor RR at (12,16)-(13,17), the
// pre-wired run c(14,16) c(15,16) c(15,15) c(15,14), fabricator F at
// (15,12)-(16,13).
// =====================================================================================

#[allow(clippy::too_many_arguments)]
fn slice2_driver(
    clock: Res<crate::simtime::SimClock>,
    mut actions: EventWriter<Action>,
    mut exit: EventWriter<AppExit>,
    power_state: Res<crate::power::PowerState>,
    cables: Res<crate::power::CableGrid>,
    fabs: Query<(&crate::power::PowerStatus, &crate::production::Fabricator)>,
    gens: Query<(Entity, &crate::power::PowerRole)>,
    buildings: Query<(Entity, &TilePos, &Building)>,
    bps_q: Query<(Entity, &TilePos, &Blueprint)>,
    racks: Query<(Entity, &TilePos, &StorageCell)>,
    stats: Res<crate::stats::Stats>,
    log: Res<EventLog>,
    mut fired: Local<Vec<&'static str>>,
    mut split_at: Local<f64>,
) {
    let Some(scenario) = std::env::var("SLICE2_SCENARIO").ok() else {
        return;
    };
    // Old-gameplay-second semantics (1 unit = 1 real s at 1×) so the
    // historically tuned scenario thresholds keep their meaning.
    let t = clock.now() / crate::simtime::BASE_SIM_RATE;

    let fab_power = || fabs.iter().next().map(|(p, _)| *p);
    let gen_e = || {
        gens.iter()
            .find(|(_, r)| matches!(r, crate::power::PowerRole::Generator { .. }))
            .map(|(e, _)| e)
    };
    let mut dump = |ctx: &str| {
        println!(
            "S2_RESULT scenario={ctx} t={t:.1} fab_power={:?} networks={:?} stats=[{}]",
            fab_power(),
            power_state.networks,
            stats.summary(),
        );
        println!("S2_LOG_BEGIN");
        for e in log
            .entries
            .iter()
            .rev()
            .take(10)
            .collect::<Vec<_>>()
            .iter()
            .rev()
        {
            println!("  [{:.1}s] {:?} {}", e.time, e.kind, e.text);
        }
        println!("S2_LOG_END");
        exit.write(AppExit::Success);
    };

    match scenario.as_str() {
        // A: healthy grid — reactor online, fabricator powered.
        "A" => {
            fire("s2a_speed", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"done") && t >= 5.0 {
                fired.push("done");
                dump("A");
            }
        }
        // B: disconnected consumer — cut the cable at the fabricator's end.
        "B" => {
            fire("s2b_cut", 0.5, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkCableDeconstruct {
                    pos: TilePos::new(15, 14),
                });
            });
            fire("s2b_speed", 0.7, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            let cut = !cables.has(TilePos::new(15, 14));
            if !fired.contains(&"done") && (t >= 10.0 && cut || t >= 120.0) {
                fired.push("done");
                dump("B");
            }
        }
        // C: grid split — build a west fabricator on the reactor side, then
        // cut the middle of the run: west stays powered, east goes dark.
        "C" => {
            fire("s2c_build", 0.4, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::PowerCable,
                    pos: TilePos::new(11, 16),
                });
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Fabricator,
                    pos: TilePos::new(9, 15),
                });
            });
            fire("s2c_speed", 0.6, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            let west_fab = buildings
                .iter()
                .any(|(_, p, b)| b.kind == BuildingKind::Fabricator && p.x == 9 && p.y == 15);
            let linked = cables.has(TilePos::new(11, 16));
            if west_fab && linked && !fired.contains(&"s2c_cut") && t >= 20.0 {
                fired.push("s2c_cut");
                let _ = actions.write(Action::MarkCableDeconstruct {
                    pos: TilePos::new(15, 15),
                });
            }
            let split = west_fab && !cables.has(TilePos::new(15, 15));
            if split && !fired.contains(&"s2c_split_seen") {
                fired.push("s2c_split_seen");
                *split_at = t;
                println!("S2_C_SPLIT_SEEN t={t:.1}");
            }
            // A second of slack so the power system (which runs after this
            // driver within the frame) reflects the cut before we dump.
            let settled = fired.contains(&"s2c_split_seen") && t >= *split_at + 1.0;
            if !fired.contains(&"done") && (settled || t >= 240.0) {
                fired.push("done");
                dump("C");
            }
        }
        // D: reconnect — cut, then re-lay the cable through construction.
        "D" => {
            fire("s2d_cut", 0.4, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkCableDeconstruct {
                    pos: TilePos::new(15, 14),
                });
            });
            fire("s2d_speed", 0.6, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            let cut = !cables.has(TilePos::new(15, 14));
            if cut && !fired.contains(&"s2d_relay") && t >= 10.0 {
                fired.push("s2d_relay");
                let _ = actions.write(Action::PlaceBlueprint {
                    kind: BuildingKind::PowerCable,
                    pos: TilePos::new(15, 14),
                });
            }
            let healed = cables.has(TilePos::new(15, 14));
            if !fired.contains(&"done")
                && (t >= 20.0 && healed && fab_power() == Some(crate::power::PowerStatus::Powered)
                    || t >= 240.0)
            {
                fired.push("done");
                dump("D");
            }
        }
        // E: generator offline — toggle standby, observe, restore.
        "E" => {
            fire("s2e_off", 0.5, t, &mut fired, &mut actions, |a| {
                if let Some(g) = gen_e() {
                    let _ = a.write(Action::SetGeneratorOn { gen: g, on: false });
                }
            });
            fire("s2e_mid", 6.0, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if t >= 12.0 && !fired.contains(&"s2e_dump1") {
                fired.push("s2e_dump1");
                println!("S2_E_OFFLINE fab_power={:?}", fab_power());
            }
            fire("s2e_on", 14.0, t, &mut fired, &mut actions, |a| {
                if let Some(g) = gen_e() {
                    let _ = a.write(Action::SetGeneratorOn { gen: g, on: true });
                }
            });
            if !fired.contains(&"done") && t >= 20.0 {
                fired.push("done");
                dump("E");
            }
        }
        // F: overload — bus out to five extra fabricators (6 x 20 > 100 PU).
        "F" => {
            if t >= 0.5 && !fired.contains(&"s2f_build") {
                fired.push("s2f_build");
                // Fabricators first: a fabricator blueprint rejects overlapping
                // blueprints, while cable blueprints may run under anything —
                // so the bus must be planned after the machines.
                // NOTE: nothing may sit on (17,10)/(18,10) — the room's only
                // door is the (17,9)->(17,10) column; a machine there seals
                // FABRICATION and starves every other blueprint inside.
                for pos in [(12, 10), (19, 10), (17, 14), (19, 14), (17, 16), (12, 12)] {
                    let _ = actions.write(Action::PlaceBlueprint {
                        kind: BuildingKind::Fabricator,
                        pos: TilePos::new(pos.0, pos.1),
                    });
                }
                // Cable bus east along row 14 plus feeder columns.
                for x in 16..=20 {
                    let _ = actions.write(Action::PlaceBlueprint {
                        kind: BuildingKind::PowerCable,
                        pos: TilePos::new(x, 14),
                    });
                }
                for y in [11, 12, 13, 15, 16, 17] {
                    let _ = actions.write(Action::PlaceBlueprint {
                        kind: BuildingKind::PowerCable,
                        pos: TilePos::new(17, y),
                    });
                    let _ = actions.write(Action::PlaceBlueprint {
                        kind: BuildingKind::PowerCable,
                        pos: TilePos::new(19, y),
                    });
                }
                // Sixth fabricator up northwest, on its own spur off the bus.
                for p in [(13, 12), (13, 13), (14, 13)] {
                    let _ = actions.write(Action::PlaceBlueprint {
                        kind: BuildingKind::PowerCable,
                        pos: TilePos::new(p.0, p.1),
                    });
                }
                // Materials for the fabricator fleet (generous: 20 parts).
                for _ in 0..20 {
                    let _ = actions.write(Action::SpawnItem {
                        kind: ItemKind::Part,
                    });
                }
            }
            fire("s2f_speed", 0.8, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            let fabs_built = buildings
                .iter()
                .filter(|(_, _, b)| b.kind == BuildingKind::Fabricator)
                .count();
            let overloaded = power_state
                .networks
                .iter()
                .any(|n| n.demand > n.generation && n.generation > 0);
            if !fired.contains(&"done") && (t >= 40.0 && overloaded || t >= 900.0) {
                fired.push("done");
                println!("S2_F fabs={fabs_built}");
                dump("F");
            }
            if !fired.contains(&"s2f_diag") && t >= 200.0 {
                fired.push("s2f_diag");
                for (e, p, bp) in bps_q.iter() {
                    if bp.kind == BuildingKind::Fabricator {
                        println!(
                            "S2_F_BP pos=({},{}) materials={} progress={:.2}",
                            p.x,
                            p.y,
                            bp.materials_label(),
                            bp.progress
                        );
                        let _ = e;
                    }
                }
                let stored: u32 = racks
                    .iter()
                    .map(|(_, _, c)| c.counts[ItemKind::Part.index()])
                    .sum();
                println!("S2_F_RACKS parts_in_racks={stored}");
            }
        }
        // G: runtime construction — isolated fabricator, then wire it up.
        "G" => {
            fire("s2g_fab", 0.4, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Fabricator,
                    pos: TilePos::new(17, 14),
                });
                for _ in 0..4 {
                    let _ = a.write(Action::SpawnItem {
                        kind: ItemKind::Part,
                    });
                }
            });
            fire("s2g_speed", 0.6, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            let built = buildings
                .iter()
                .any(|(_, p, b)| b.kind == BuildingKind::Fabricator && p.x == 17 && p.y == 14);
            if built && !fired.contains(&"s2g_isolated") {
                fired.push("s2g_isolated");
                println!("S2_G_ISOLATED fab_power={:?}", fab_power_at(&fabs, 17, 14));
            }
            if built && !fired.contains(&"s2g_wire") && t >= 10.0 {
                fired.push("s2g_wire");
                let _ = actions.write(Action::PlaceBlueprint {
                    kind: BuildingKind::PowerCable,
                    pos: TilePos::new(16, 14),
                });
                let _ = actions.write(Action::PlaceBlueprint {
                    kind: BuildingKind::PowerCable,
                    pos: TilePos::new(17, 14),
                });
            }
            let wired = cables.has(TilePos::new(17, 14)) && cables.has(TilePos::new(16, 14));
            if !fired.contains(&"done") && (t >= 20.0 && wired || t >= 240.0) {
                fired.push("done");
                println!("S2_G_WIRED fab_power={:?}", fab_power_at(&fabs, 17, 14));
                dump("G");
            }
        }
        // H: runtime demolition — tear the middle of the run out.
        "H" => {
            fire("s2h_cut", 0.5, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkCableDeconstruct {
                    pos: TilePos::new(15, 15),
                });
            });
            fire("s2h_speed", 0.7, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            let gone = !cables.has(TilePos::new(15, 15));
            if !fired.contains(&"done") && (t >= 10.0 && gone || t >= 120.0) {
                fired.push("done");
                dump("H");
            }
        }
        // I: time controls — cycle pause/1x/2x/4x, grid stays consistent.
        "I" => {
            fire("s2i_1", 0.5, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 1 });
            });
            fire("s2i_2", 3.0, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 2 });
            });
            fire("s2i_3", 6.0, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            fire("s2i_0", 9.0, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 0 });
            });
            if !fired.contains(&"done") && t >= 12.0 {
                fired.push("done");
                dump("I");
            }
        }
        // J: regression marker — the Slice 0/1 suites are SLICE0_SCENARIO,
        // rerun them separately; this just confirms the powered ship hauls.
        "J" => {
            fire("s2j_mark", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            fire("s2j_order", 0.5, t, &mut fired, &mut actions, |a| {
                let _ = a;
            });
            fire("s2j_speed", 0.7, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"done") && t >= 30.0 {
                fired.push("done");
                dump("J");
            }
        }
        // PW: player-perspective walkthrough — open the power view, watch a
        // blackout and recovery, dump the world for the report.
        "PW" => {
            fire("pw_view", 0.5, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::CycleOverlay);
            });
            fire("pw_cut", 1.0, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkCableDeconstruct {
                    pos: TilePos::new(15, 15),
                });
            });
            fire("pw_speed", 1.2, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            let cut = !cables.has(TilePos::new(15, 15));
            if cut && !fired.contains(&"pw_dark") && t >= 6.0 {
                fired.push("pw_dark");
                println!(
                    "PW_DARK t={t:.1} fab_power={:?} networks={:?}",
                    fab_power(),
                    power_state.networks
                );
                // Player re-lays the missing link.
                let _ = actions.write(Action::PlaceBlueprint {
                    kind: BuildingKind::PowerCable,
                    pos: TilePos::new(15, 15),
                });
            }
            let healed = cables.has(TilePos::new(15, 15))
                && fab_power() == Some(crate::power::PowerStatus::Powered);
            if !fired.contains(&"done") && (t >= 10.0 && healed || t >= 120.0) {
                fired.push("done");
                dump("PW");
            }
        }
        _ => {}
    }
    let _ = (&cables, &buildings);
}

fn fab_power_at(
    fabs: &Query<(&crate::power::PowerStatus, &crate::production::Fabricator)>,
    _x: i32,
    _y: i32,
) -> Option<crate::power::PowerStatus> {
    fabs.iter().next().map(|(p, _)| *p)
}

// =====================================================================================
// Slice 3 acceptance scenarios (Thermal & Cooling), driven by SLICE3_SCENARIO=A..H,R.
// The starter coolant loop: H(14,17) K(15,17) p(16,17) Z(17,17) p(18,17) Z(19,17)
// p(20,17) W(21,17) p(21,16) p(20,16) p(19,16) p(18,16) p(17,16) p(16,16).
// Heavy numeric verification (conservation, hysteresis, equivalence) lives in
// tests/thermal.rs; these scenarios drive the *full app* wiring end to end.
// =====================================================================================

#[allow(clippy::too_many_arguments)]
fn slice3_driver(
    clock: Res<crate::simtime::SimClock>,
    mut actions: EventWriter<Action>,
    mut exit: EventWriter<AppExit>,
    thermal_grid: Res<crate::thermal::ThermalGrid>,
    tstats: Res<crate::thermal::ThermalStats>,
    coolant: Res<crate::coolant::CoolantState>,
    water: Res<crate::coolant::WaterGrid>,
    cstats: Res<crate::coolant::CoolantStats>,
    power_state: Res<crate::power::PowerState>,
    overlay: Res<crate::OverlayMode>,
    reactors: Query<(
        &crate::building::Footprint,
        &crate::thermal::ThermalState,
        &crate::power::PowerRole,
    )>,
    pumps: Query<
        (&crate::building::Footprint, &crate::power::PowerStatus),
        With<crate::coolant::Pump>,
    >,
    stats: Res<crate::stats::Stats>,
    log: Res<EventLog>,
    mut fired: Local<Vec<&'static str>>,
) {
    let Some(scenario) = std::env::var("SLICE3_SCENARIO").ok() else {
        return;
    };
    // Ship seconds — thermal pacing is in sim time.
    let t = clock.now();

    let core = || {
        reactors
            .iter()
            .next()
            .map(|(foot, state, role)| (thermal_grid.max_footprint_temp(foot), *state, *role))
    };
    let mut dump = |ctx: &str| {
        let (temp, state, role) = core().unwrap_or((
            f32::NAN,
            crate::thermal::ThermalState::Normal,
            crate::power::PowerRole::consumer(0),
        ));
        let pump = pumps
            .iter()
            .next()
            .map(|(f, p)| ((f.x, f.y), *p, water.amount_at(TilePos::new(f.x, f.y))));
        println!(
            "S3_RESULT scenario={ctx} t={t:.0}s core={temp:.1}C state={state:?} role={role:?} pump={pump:?} coolants={:?} spilled={:.2} injected={:.0} radiated={:.0} overlay={:?} nets={:?} stats=[{}]",
            coolant.networks,
            cstats.spilled_water,
            tstats.injected_total,
            tstats.radiated_total,
            *overlay,
            power_state.networks,
            stats.summary(),
        );
        println!("S3_LOG_BEGIN");
        for e in log
            .entries
            .iter()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
            .iter()
            .rev()
        {
            println!("  [{:.0}s] {:?} {}", e.time, e.kind, e.text);
        }
        println!("S3_LOG_END");
        exit.write(AppExit::Success);
    };

    match scenario.as_str() {
        // A: boot stability — 90 ship minutes at 4x with the stock load: the
        // reactor must stay cool, the loop must radiate, water must hold.
        "A" => {
            fire("s3a_speed", 0.2, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"done") && t >= 5400.0 {
                fired.push("done");
                dump("A");
            }
        }
        // B: cooling failure cascade — cut the ring, let the crisis develop,
        // repair, and confirm recovery. The deadlock guarantee (emergency
        // power keeps the pump) shows in the dump.
        "B" => {
            fire("s3b_speed", 0.2, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            fire("s3b_cut", 30.0, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkPipeDeconstruct {
                    pos: TilePos::new(16, 17),
                });
            });
            fire("s3b_fix", 2600.0, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::CoolantPipe,
                    pos: TilePos::new(16, 17),
                });
            });
            // Healed = the ring is whole again (one network carrying both
            // the pump and the radiators) AND the core is back to Normal.
            let healed = core().is_some_and(|(_, s, _)| s == crate::thermal::ThermalState::Normal)
                && coolant.networks.len() == 1
                && coolant
                    .networks
                    .iter()
                    .any(|n| n.powered_pumps > 0 && n.radiators > 0 && n.flow > 0.0);
            if !fired.contains(&"done") && ((healed && t >= 3400.0) || t >= 5400.0) {
                fired.push("done");
                dump("B");
            }
        }
        // C: pump power dependency — cut the cable that feeds the loop's
        // pump; circulation stops (stagnant), restore it, flow returns.
        "C" => {
            fire("s3c_speed", 0.2, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            fire("s3c_cut", 30.0, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkCableDeconstruct {
                    pos: TilePos::new(15, 16),
                });
            });
            fire("s3c_fix", 700.0, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::PowerCable,
                    pos: TilePos::new(15, 16),
                });
            });
            if !fired.contains(&"done") && t >= 1600.0 {
                fired.push("done");
                dump("C");
            }
        }
        // E: water preservation — tear a pipe down next to the reservoir and
        // confirm the water moved into the network instead of vanishing.
        "E" => {
            fire("s3e_speed", 0.2, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"water0") && t >= 1.0 {
                fired.push("water0");
                println!("S3_E_WATER0 total={:.1}", water.total_water());
            }
            fire("s3e_cut", 30.0, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkPipeDeconstruct {
                    pos: TilePos::new(20, 17),
                });
            });
            if !fired.contains(&"done") && t >= 1000.0 {
                fired.push("done");
                println!(
                    "S3_E_WATER1 total={:.1} spilled={:.2}",
                    water.total_water(),
                    cstats.spilled_water
                );
                dump("E");
            }
        }
        // F: overlay modes cycle through Off → Power → Thermal → Coolant.
        "F" => {
            fire("s3f_1", 1.0, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::CycleOverlay);
            });
            fire("s3f_2", 2.0, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::CycleOverlay);
            });
            fire("s3f_3", 3.0, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::CycleOverlay);
            });
            if !fired.contains(&"done") && t >= 4.0 {
                fired.push("done");
                dump("F");
            }
        }
        // V: visual smoke — cycle the overlay SLICE3_VIEW_N times and stay
        // alive (paired with SLICE0_SHOT / SLICE0_SMOKE).
        "V" => {
            let n: usize = std::env::var("SLICE3_VIEW_N")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            fire("s3v_speed", 0.2, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            for i in 0..n.min(8) {
                let tag: &'static str = Box::leak(format!("s3v_c{i}").into_boxed_str());
                let at = 1.0 + i as f64;
                fire(tag, at, t, &mut fired, &mut actions, |a| {
                    let _ = a.write(Action::CycleOverlay);
                });
            }
        }
        // R: full-stack regression snapshot at 4x with thermal running.
        "R" => {
            fire("s3r_speed", 0.2, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"done") && t >= 600.0 {
                fired.push("done");
                dump("R");
            }
        }
        _ => {}
    }
}

// =====================================================================================
// Slice 4 acceptance scenarios (Airtight Compartments & Doors), driven by
// SLICE4_SCENARIO. Preinstalled doors: (6,6) CARGO, (16,6) CREW, (28,6) ORE
// BAY, (5,9) PARTS, (17,9) FABRICATION. STORAGE keeps both corridor gaps
// ((27,9)/(32,9)) open. Heavy numeric verification (conservation, thermal
// isolation rates, cache behavior, perf) lives in tests/airtight.rs; these
// drive the full app wiring end to end.
// =====================================================================================

#[allow(clippy::too_many_arguments)]
fn slice4_driver(
    clock: Res<crate::simtime::SimClock>,
    mut actions: EventWriter<Action>,
    mut exit: EventWriter<AppExit>,
    map: Res<crate::map::ShipMap>,
    comps: Res<crate::airtight::Compartments>,
    thermal: Res<crate::thermal::ThermalGrid>,
    doors: Query<(Entity, &TilePos, &crate::airtight::Door)>,
    crews: Query<(Entity, &Crew, &CrewTask, &TilePos), With<Crew>>,
    marked: Query<(Entity, &Item), With<MarkedForHaul>>,
    racks: Query<(Entity, &TilePos, &StorageCell)>,
    buildings: Query<(Entity, &TilePos, &Building)>,
    stats: Res<crate::stats::Stats>,
    log: Res<EventLog>,
    mut fired: Local<Vec<&'static str>>,
    mut phase_seq: Local<Vec<&'static str>>,
) {
    let Some(scenario) = std::env::var("SLICE4_SCENARIO").ok() else {
        return;
    };
    // Old-gameplay-second semantics so windows stay comparable with the
    // earlier slice drivers (1 unit = 1 real s at 1x).
    let t = clock.now() / crate::simtime::BASE_SIM_RATE;
    let door_at = |x: i32, y: i32| {
        doors
            .iter()
            .find(|(_, p, _)| p.x == x && p.y == y)
            .map(|(e, _, d)| (e, d.phase, d.progress, d.mode, d.cycles))
    };
    let mut dump = |ctx: &str| {
        let door_dump: Vec<String> = doors
            .iter()
            .map(|(_, p, d)| {
                format!(
                    "({},{})={} {} {} cycles={}",
                    p.x,
                    p.y,
                    d.phase.label(),
                    d.mode.label(),
                    d.axis.label(),
                    d.cycles
                )
            })
            .collect();
        println!(
            "S4_RESULT scenario={ctx} t={t:.1} regions={} sealed={} exposed={} air_groups={} portals={} rebuilds={} air_recomputes={} doors=[{}] stats=[{}]",
            comps.regions.len(),
            comps.sealed_count(),
            comps.exposed_count(),
            comps.air_groups,
            comps.doors.len(),
            comps.rebuilds,
            comps.air_recomputes,
            door_dump.join(", "),
            stats.summary(),
        );
        println!("S4_LOG_BEGIN");
        for e in log
            .entries
            .iter()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
            .iter()
            .rev()
        {
            println!("  [{:.0}s] {:?} {}", e.time, e.kind, e.text);
        }
        println!("S4_LOG_END");
        exit.write(AppExit::Success);
    };

    match scenario.as_str() {
        // A - starter compartments: boots with 6 sealed compartments, five
        // working preinstalled doors, and hauls keep flowing through them.
        "A" => {
            fire("s4a_mark", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            fire("s4a_speed", 0.6, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            let stored: u32 = racks.iter().map(|(_, _, s)| s.stored()).sum();
            if !fired.contains(&"done") && ((t >= 40.0 && stored >= 20) || t >= 200.0) {
                fired.push("done");
                println!(
                    "S4_A_COMPARTMENTS regions={} sealed={} exposed={} stored={stored}",
                    comps.regions.len(),
                    comps.sealed_count(),
                    comps.exposed_count(),
                );
                dump("A");
            }
        }
        // B - auto door passage: one door cycles Closed -> Opening -> Open
        // -> (crew passes) -> Closing -> Closed while hauling runs.
        "B" => {
            fire("s4b_mark", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            fire("s4b_speed", 0.5, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if let Some((_, phase, ..)) = door_at(6, 6) {
                let label = match phase {
                    crate::airtight::DoorPhase::Closed => "Closed",
                    crate::airtight::DoorPhase::Opening => "Opening",
                    crate::airtight::DoorPhase::Open => "Open",
                    crate::airtight::DoorPhase::Closing => "Closing",
                };
                if phase_seq.last().copied() != Some(label) {
                    phase_seq.push(label);
                }
            }
            let saw_all = phase_seq.contains(&"Opening")
                && phase_seq.contains(&"Open")
                && phase_seq.contains(&"Closing")
                && phase_seq.last() == Some(&"Closed");
            if !fired.contains(&"done") && ((t >= 8.0 && saw_all) || t >= 200.0) {
                fired.push("done");
                println!("S4_B_PHASE_SEQ {:?}", phase_seq);
                dump("B");
            }
        }
        // C - multiple crew drain: a stream of haulers must pass one door
        // without open/close flapping (cycles stay low while hauls rack up).
        "C" => {
            fire("s4c_mark", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            fire("s4c_spawn", 0.5, t, &mut fired, &mut actions, |a| {
                for _ in 0..10 {
                    let _ = a.write(Action::SpawnItem {
                        kind: ItemKind::Ore,
                    });
                }
            });
            fire("s4c_speed", 0.7, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"done") && t >= 120.0 {
                fired.push("done");
                let (cycles, phase) = door_at(6, 6)
                    .map(|(_, ph, _, _, c)| (c, ph))
                    .unwrap_or((0, crate::airtight::DoorPhase::Closed));
                println!(
                    "S4_C_DRAIN hauls={} door_cycles={cycles} final_phase={:?} marked_left={}",
                    stats.hauls_done,
                    phase,
                    marked.iter().count()
                );
                dump("C");
            }
        }
        // D - Hold Open: door opens, stays open, air groups merge, traffic
        // flows without interruption.
        "D" => {
            fire("s4d_hold", 0.8, t, &mut fired, &mut actions, |a| {
                if let Some((e, ..)) = door_at(6, 6) {
                    let _ = a.write(Action::SetDoorMode {
                        door: e,
                        mode: crate::airtight::DoorMode::HoldOpen,
                    });
                }
            });
            fire("s4d_mark", 1.2, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            fire("s4d_speed", 1.5, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            let open = door_at(6, 6).is_some_and(|(_, _, p, _, _)| p >= 1.0);
            let merged = comps.air_groups < comps.regions.len() as u16;
            if !fired.contains(&"done") && ((t >= 30.0 && open && merged) || t >= 120.0) {
                fired.push("done");
                println!(
                    "S4_D_HOLD open={open} air_groups={}/{}",
                    comps.air_groups,
                    comps.regions.len()
                );
                dump("D");
            }
        }
        // E - Lock Closed: the cargo door becomes a wall; marked items behind
        // it are unreachable, no crew squeezes through, no claim thrash.
        "E" => {
            fire("s4e_lock", 0.8, t, &mut fired, &mut actions, |a| {
                if let Some((e, ..)) = door_at(6, 6) {
                    let _ = a.write(Action::SetDoorMode {
                        door: e,
                        mode: crate::airtight::DoorMode::LockClosed,
                    });
                }
            });
            fire("s4e_mark", 1.5, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            fire("s4e_speed", 2.0, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            let cargo_region = comps.region_at(TilePos::new(5, 3));
            let crew_inside = crews
                .iter()
                .filter(|(_, _, _, p)| comps.region_at(**p) == cargo_region)
                .count();
            if !fired.contains(&"done") && t >= 60.0 {
                fired.push("done");
                println!(
                    "S4_E_LOCK marked_left={} crew_inside_cargo={crew_inside} hauling_still={}",
                    marked.iter().count(),
                    crews
                        .iter()
                        .filter(|(_, _, task, _)| matches!(task, CrewTask::Haul(_)))
                        .count(),
                );
                dump("E");
            }
        }
        // F - structural split: wall both STORAGE corridor gaps; the storage
        // bay becomes its own sealed compartment.
        "F" => {
            fire("s4f_wall", 0.4, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Wall,
                    pos: TilePos::new(27, 9),
                });
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Wall,
                    pos: TilePos::new(32, 9),
                });
            });
            fire("s4f_speed", 0.7, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            let walls_built = buildings
                .iter()
                .filter(|(_, p, b)| {
                    b.kind == BuildingKind::Wall && p.y == 9 && (p.x == 27 || p.x == 32)
                })
                .count();
            let split = comps.regions.len() >= 7;
            if !fired.contains(&"done") && ((t >= 30.0 && walls_built == 2 && split) || t >= 200.0)
            {
                fired.push("done");
                println!(
                    "S4_F_SPLIT walls={walls_built} regions={} rebuilds={}",
                    comps.regions.len(),
                    comps.rebuilds
                );
                dump("F");
            }
        }
        // G - structural merge: after the F split, tear one wall back out.
        "G" => {
            fire("s4g_wall", 0.4, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Wall,
                    pos: TilePos::new(27, 9),
                });
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Wall,
                    pos: TilePos::new(32, 9),
                });
            });
            fire("s4g_speed", 0.7, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            let both = buildings
                .iter()
                .filter(|(_, p, b)| {
                    b.kind == BuildingKind::Wall && p.y == 9 && (p.x == 27 || p.x == 32)
                })
                .count()
                == 2;
            if both && !fired.contains(&"s4g_demo") && t >= 15.0 {
                if let Some((e, _, _)) = buildings
                    .iter()
                    .find(|(_, p, b)| b.kind == BuildingKind::Wall && p.x == 27 && p.y == 9)
                {
                    fired.push("s4g_demo");
                    let _ = actions.write(Action::MarkDeconstruct { building: e });
                }
            }
            let merged = comps.regions.len() == 7;
            if !fired.contains(&"done")
                && ((t >= 40.0 && fired.contains(&"s4g_demo") && merged) || t >= 200.0)
            {
                fired.push("done");
                println!(
                    "S4_G_MERGE regions={} rebuilds={}",
                    comps.regions.len(),
                    comps.rebuilds
                );
                dump("G");
            }
        }
        // H - build a door: wall one STORAGE gap, door the other; the door
        // must resolve N-S orientation, seal when closed, and pass crew.
        "H" => {
            fire("s4h_wall", 0.4, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Wall,
                    pos: TilePos::new(27, 9),
                });
            });
            fire("s4h_door", 0.6, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Door,
                    pos: TilePos::new(32, 9),
                });
            });
            fire("s4h_mark", 1.0, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            fire("s4h_speed", 1.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            let door = doors.iter().find(|(_, p, _)| p.x == 32 && p.y == 9);
            let stored: u32 = racks.iter().map(|(_, _, s)| s.stored()).sum();
            if !fired.contains(&"done")
                && ((t >= 60.0 && door.is_some() && stored >= 10) || t >= 240.0)
            {
                fired.push("done");
                let (axis, sealed) = door
                    .map(|(_, _, d)| (d.axis, d.sealed()))
                    .unwrap_or((crate::airtight::DoorAxis::Ns, true));
                println!(
                    "S4_H_BUILD door_axis={} sealed={sealed} regions={} stored={stored}",
                    axis.label(),
                    comps.regions.len(),
                );
                dump("H");
            }
        }
        // I - door demolition: after the H setup, tear the door out; the
        // portal disappears and the regions merge permanently.
        "I" => {
            fire("s4i_wall", 0.4, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Wall,
                    pos: TilePos::new(27, 9),
                });
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Door,
                    pos: TilePos::new(32, 9),
                });
            });
            fire("s4i_speed", 0.7, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            let door_built = doors.iter().any(|(_, p, _)| p.x == 32 && p.y == 9);
            let storage_sealed = comps.regions.len() >= 7;
            if door_built && storage_sealed && !fired.contains(&"s4i_demo") && t >= 20.0 {
                if let Some((e, _, _)) = doors.iter().find(|(_, p, _)| p.x == 32 && p.y == 9) {
                    fired.push("s4i_demo");
                    let _ = actions.write(Action::MarkDeconstruct { building: e });
                }
            }
            let door_gone = doors.iter().all(|(_, p, _)| !(p.x == 32 && p.y == 9));
            let merged = comps.regions.len() == 7;
            if !fired.contains(&"done")
                && ((t >= 50.0 && fired.contains(&"s4i_demo") && door_gone && merged) || t >= 240.0)
            {
                fired.push("done");
                println!(
                    "S4_I_DEMO door_gone={door_gone} portals={} regions={} boundary_open={}",
                    comps.doors.len(),
                    comps.regions.len(),
                    crate::airtight::boundary(&map, TilePos::new(32, 8), TilePos::new(32, 9))
                        == crate::airtight::Boundary::Open,
                );
                dump("I");
            }
        }
        // J - thermal isolation: FABRICATION heats while its door stays
        // closed; the corridor must not follow (no fast ambient mixing).
        "J" => {
            fire("s4j_order", 0.4, t, &mut fired, &mut actions, |a| {
                // Order the starter fabricator: repeat production keeps heat
                // flowing. The fabs query is not available here, so drive it
                // through a hauler-friendly order via the first fab entity
                // found by the generic building query.
                if let Some((e, _, _)) = buildings
                    .iter()
                    .find(|(_, _, b)| b.kind == BuildingKind::Fabricator)
                {
                    let _ = a.write(Action::FabAddOrder { fab: e, batches: 5 });
                }
            });
            fire("s4j_speed", 0.8, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"s4j_t1") && t >= 45.0 {
                fired.push("s4j_t1");
                println!(
                    "S4_J_T1 t={t:.0} fab={:.1}C corridor={:.1}C door_closed={}",
                    thermal.amb_at(TilePos::new(14, 12)),
                    thermal.amb_at(TilePos::new(14, 7)),
                    door_at(17, 9).is_none_or(|(_, _, p, _, _)| p < 1.0),
                );
            }
            if !fired.contains(&"done") && t >= 90.0 {
                fired.push("done");
                let (fab, corridor) = (
                    thermal.amb_at(TilePos::new(14, 12)),
                    thermal.amb_at(TilePos::new(14, 7)),
                );
                println!(
                    "S4_J_ISOLATED fab={fab:.1}C corridor={corridor:.1}C delta={:.1}",
                    fab - corridor
                );
                dump("J");
            }
        }
        // K - thermal connection: hold the fabrication door open; heat from
        // the hot room starts spreading into the corridor.
        "K" => {
            fire("s4k_order", 0.4, t, &mut fired, &mut actions, |a| {
                if let Some((e, _, _)) = buildings
                    .iter()
                    .find(|(_, _, b)| b.kind == BuildingKind::Fabricator)
                {
                    let _ = a.write(Action::FabAddOrder { fab: e, batches: 5 });
                }
            });
            fire("s4k_hold", 0.6, t, &mut fired, &mut actions, |a| {
                if let Some((e, ..)) = door_at(17, 9) {
                    let _ = a.write(Action::SetDoorMode {
                        door: e,
                        mode: crate::airtight::DoorMode::HoldOpen,
                    });
                }
            });
            fire("s4k_speed", 1.0, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"s4k_t1") && t >= 30.0 {
                fired.push("s4k_t1");
                println!(
                    "S4_K_T1 t={t:.0} fab={:.1}C corridor={:.1}C",
                    thermal.amb_at(TilePos::new(14, 12)),
                    thermal.amb_at(TilePos::new(14, 7)),
                );
            }
            if !fired.contains(&"done") && t >= 75.0 {
                fired.push("done");
                let (fab, corridor) = (
                    thermal.amb_at(TilePos::new(14, 12)),
                    thermal.amb_at(TilePos::new(14, 7)),
                );
                println!(
                    "S4_K_CONNECTED fab={fab:.1}C corridor={corridor:.1}C delta={:.1}",
                    fab - corridor
                );
                dump("K");
            }
        }
        // L - re-close: direct exchange stops again; no temperature resets.
        "L" => {
            fire("s4l_order", 0.4, t, &mut fired, &mut actions, |a| {
                if let Some((e, _, _)) = buildings
                    .iter()
                    .find(|(_, _, b)| b.kind == BuildingKind::Fabricator)
                {
                    let _ = a.write(Action::FabAddOrder { fab: e, batches: 5 });
                }
            });
            fire("s4l_open", 0.5, t, &mut fired, &mut actions, |a| {
                if let Some((e, ..)) = door_at(17, 9) {
                    let _ = a.write(Action::SetDoorMode {
                        door: e,
                        mode: crate::airtight::DoorMode::HoldOpen,
                    });
                }
            });
            fire("s4l_speed", 0.8, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"s4l_close") && t >= 25.0 {
                fired.push("s4l_close");
                if let Some((e, ..)) = door_at(17, 9) {
                    let _ = actions.write(Action::SetDoorMode {
                        door: e,
                        mode: crate::airtight::DoorMode::Auto,
                    });
                }
                println!(
                    "S4_L_CLOSING fab={:.1}C corridor={:.1}C",
                    thermal.amb_at(TilePos::new(14, 12)),
                    thermal.amb_at(TilePos::new(14, 7)),
                );
            }
            if !fired.contains(&"done") && t >= 70.0 {
                fired.push("done");
                let (fab, corridor) = (
                    thermal.amb_at(TilePos::new(14, 12)),
                    thermal.amb_at(TilePos::new(14, 7)),
                );
                println!(
                    "S4_L_RECLOSED fab={fab:.1}C corridor={corridor:.1}C delta={:.1}",
                    fab - corridor
                );
                dump("L");
            }
        }
        // N - stable cache: long run, no geometry/door changes -> zero
        // structural rebuilds, no spurious air recomputes.
        "N" => {
            fire("s4n_speed", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"done") && t >= 90.0 {
                fired.push("done");
                println!(
                    "S4_N_CACHE rebuilds={} air_recomputes={} regions={}",
                    comps.rebuilds,
                    comps.air_recomputes,
                    comps.regions.len()
                );
                dump("N");
            }
        }
        // O - door toggle performance: flap a door's mode constantly; the
        // structural partition must not rebuild (only air recompute).
        "O" => {
            fire("s4o_speed", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            for i in 0..24 {
                let tag: &'static str = Box::leak(format!("s4o_t{i}").into_boxed_str());
                let at = 1.0 + i as f64 * 1.2;
                fire(tag, at, t, &mut fired, &mut actions, |a| {
                    if let Some((e, _, _, mode, _)) = door_at(6, 6) {
                        let next = match mode {
                            crate::airtight::DoorMode::Auto
                            | crate::airtight::DoorMode::LockClosed => {
                                crate::airtight::DoorMode::HoldOpen
                            }
                            crate::airtight::DoorMode::HoldOpen => {
                                crate::airtight::DoorMode::LockClosed
                            }
                        };
                        let _ = a.write(Action::SetDoorMode {
                            door: e,
                            mode: next,
                        });
                    }
                });
            }
            if !fired.contains(&"done") && t >= 40.0 {
                fired.push("done");
                println!(
                    "S4_O_TOGGLE rebuilds={} air_recomputes={}",
                    comps.rebuilds, comps.air_recomputes
                );
                dump("O");
            }
        }
        // P - time equivalence: run at SLICE4_SPEED (1|2|4) and dump the
        // door/compartment state at a fixed sim time; compare across runs.
        "P" => {
            let idx: usize = std::env::var("SLICE4_SPEED")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            fire("s4p_speed", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: idx });
            });
            if !fired.contains(&"done") && clock.now() >= 6000.0 {
                fired.push("done");
                let door_states: Vec<String> = doors
                    .iter()
                    .map(|(_, p, d)| {
                        format!("({},{})={} {:.2}", p.x, p.y, d.phase.label(), d.progress)
                    })
                    .collect();
                println!(
                    "S4_P_EQ speed={idx} sim_t={:.0} doors=[{}] regions={} air_groups={}",
                    clock.now(),
                    door_states.join(", "),
                    comps.regions.len(),
                    comps.air_groups,
                );
                dump("P");
            }
        }
        _ => {}
    }
}

// =====================================================================================
// SLICE5_SCENARIO. Atmosphere acceptance driver: A (stable boot), B (closed-
// door isolation), C (open-door equalization), D (auto-door transient), E
// (composition mixing), F (pollutant spreading), G (breach decompression),
// H (emergency isolation), I (re-open after isolation), O (pause freeze),
// P (speed equivalence), Q (sleep/wake). Heavy numeric verification
// (conservation, formula properties, perf) lives in tests/atmosphere.rs;
// these drive the full app wiring end to end.
// =====================================================================================

fn s5_region_tiles(
    map: &crate::map::ShipMap,
    comps: &crate::airtight::Compartments,
    rid: u16,
) -> Vec<TilePos> {
    let mut out = Vec::new();
    for y in 0..map.height {
        for x in 0..map.width {
            if comps.id[(y * map.width + x) as usize] == rid {
                out.push(TilePos::new(x, y));
            }
        }
    }
    out
}

fn s5_region_at(comps: &crate::airtight::Compartments, p: TilePos) -> u16 {
    comps.region_at(p)
}

/// (avg, min, max) total pressure over a region's tiles.
fn s5_region_pressure(
    atmo: &crate::atmosphere::AtmosphereGrid,
    thermal: &crate::thermal::ThermalGrid,
    tiles: &[TilePos],
) -> (f32, f32, f32) {
    let mut sum = 0.0;
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for &p in tiles {
        let v = atmo.pressure_at(p, thermal);
        sum += v;
        min = min.min(v);
        max = max.max(v);
    }
    if tiles.is_empty() {
        (0.0, 0.0, 0.0)
    } else {
        (sum / tiles.len() as f32, min, max)
    }
}

/// Species-average mole fractions of a region (for composition prints).
fn s5_region_fractions(atmo: &crate::atmosphere::AtmosphereGrid, tiles: &[TilePos]) -> [f32; 4] {
    let mut mol = [0.0f32; 4];
    for &p in tiles {
        let m = atmo.mixture_at(p);
        for (dst, src) in mol.iter_mut().zip(m.mol.iter()) {
            *dst += *src;
        }
    }
    let total: f32 = mol.iter().sum();
    if total <= 0.0 {
        [0.0; 4]
    } else {
        [
            mol[0] / total,
            mol[1] / total,
            mol[2] / total,
            mol[3] / total,
        ]
    }
}

#[allow(clippy::too_many_arguments)]
fn slice5_driver(
    clock: Res<crate::simtime::SimClock>,
    mut exit: EventWriter<AppExit>,
    mut map: ResMut<crate::map::ShipMap>,
    mut thermal: ResMut<crate::thermal::ThermalGrid>,
    mut atmo: ResMut<crate::atmosphere::AtmosphereGrid>,
    mut astats: ResMut<crate::atmosphere::AtmoStats>,
    comps: Res<crate::airtight::Compartments>,
    mut doors: Query<(&TilePos, &mut crate::airtight::Door)>,
    mut demand: ResMut<crate::airtight::DoorDemand>,
    mut speed: ResMut<crate::time_ctrl::GameSpeed>,
    mut fired: Local<Vec<&'static str>>,
) {
    let Some(scenario) = std::env::var("SLICE5_SCENARIO").ok() else {
        return;
    };
    let t = clock.now() / crate::simtime::BASE_SIM_RATE;
    let cargo = s5_region_at(&comps, TilePos::new(2, 2));
    let crew = s5_region_at(&comps, TilePos::new(18, 2));
    let corridor = s5_region_at(&comps, TilePos::new(10, 7));
    let mut set_door_mode = |pos: TilePos, mode: crate::airtight::DoorMode| {
        for (p, mut d) in doors.iter_mut() {
            if *p == pos {
                d.mode = mode;
            }
        }
    };
    let totals = |atmo: &crate::atmosphere::AtmosphereGrid| -> [f64; 4] {
        crate::atmosphere::SPECIES.map(|s| atmo.onboard(s))
    };
    let boot = astats.boot_mol;

    match scenario.as_str() {
        // A - stable starter atmosphere: pressure + species hold, gas
        // conserved, active workload settles to the heated room only.
        "A" => {
            if !fired.contains(&"speed") {
                fired.push("speed");
                speed.index = 3;
            }
            if !fired.contains(&"boot") && t >= 0.5 {
                fired.push("boot");
                let (avg, min, max) =
                    s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, corridor));
                println!(
                    "S5_A_BOOT t={t:.1} pressure avg={avg:.1} min={min:.1} max={max:.1} kPa active={} species_total={:.1}",
                    atmo.awake_count(),
                    boot.iter().sum::<f64>(),
                );
            }
            if !fired.contains(&"done") && t >= 120.0 {
                fired.push("done");
                let now = totals(&atmo);
                let (avg, min, max) =
                    s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, corridor));
                let drift: f64 = now
                    .iter()
                    .zip(boot.iter())
                    .map(|(a, b)| (a - b).abs())
                    .sum();
                println!(
                    "S5_A_STABLE t={t:.1} pressure avg={avg:.1} min={min:.1} max={max:.1} drift_mol={drift:.4} vented={:.2} active={}",
                    astats.vented_mol.iter().sum::<f64>(),
                    atmo.awake_count(),
                );
            }
            // Long soak: the workload settles once the ship reaches thermal
            // equilibrium (8 sim-hours).
            if !fired.contains(&"soak") && t >= 480.0 {
                fired.push("soak");
                let (avg, min, max) =
                    s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, corridor));
                println!(
                    "S5_A_SOAK t={t:.1} pressure avg={avg:.1} min={min:.1} max={max:.1} vented={:.2} active={}",
                    astats.vented_mol.iter().sum::<f64>(),
                    atmo.awake_count(),
                );
                exit.write(AppExit::Success);
            }
        }
        // B - closed-door pressure isolation: CARGO dropped to ~half stays
        // put while the door is closed.
        "B" => {
            if !fired.contains(&"lower") && t >= 0.5 {
                fired.push("lower");
                for p in s5_region_tiles(&map, &comps, cargo) {
                    let removed = atmo.remove_fraction(p, 0.5);
                    astats.debug_removed(&removed);
                }
            }
            if !fired.contains(&"done") && t >= 30.0 {
                fired.push("done");
                let (ca, _, _) =
                    s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, cargo));
                let (cr, _, _) =
                    s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, crew));
                println!(
                    "S5_B_ISOLATED t={t:.1} cargo={ca:.1} kPa crew={cr:.1} kPa (door closed — expect ~50 vs ~101)"
                );
                exit.write(AppExit::Success);
            }
        }
        // C - open-door equalization: HoldOpen, front propagates from the
        // door outward, no instant room-average.
        "C" => {
            if !fired.contains(&"lower") && t >= 0.5 {
                fired.push("lower");
                for p in s5_region_tiles(&map, &comps, cargo) {
                    let removed = atmo.remove_fraction(p, 0.5);
                    astats.debug_removed(&removed);
                }
            }
            if !fired.contains(&"open") && t >= 1.0 {
                fired.push("open");
                set_door_mode(TilePos::new(6, 6), crate::airtight::DoorMode::HoldOpen);
            }
            for (tag, at) in [("c1", 5.0), ("c2", 15.0)] {
                if !fired.contains(&tag) && t >= at {
                    fired.push(tag);
                    let near = atmo.pressure_at(TilePos::new(6, 5), &thermal);
                    let far = atmo.pressure_at(TilePos::new(1, 1), &thermal);
                    let (ca, _, _) =
                        s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, cargo));
                    let (co, _, _) = s5_region_pressure(
                        &atmo,
                        &thermal,
                        &s5_region_tiles(&map, &comps, corridor),
                    );
                    println!(
                        "S5_C_{tag} t={t:.1} near_door={near:.1} far_cargo={far:.1} cargo_avg={ca:.1} corridor_avg={co:.1} kPa"
                    );
                }
            }
            if !fired.contains(&"done") && t >= 90.0 {
                fired.push("done");
                let (ca, cmin, cmax) =
                    s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, cargo));
                let (co, _, _) =
                    s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, corridor));
                let now = totals(&atmo);
                let sum = now.iter().sum::<f64>() + astats.vented_mol.iter().sum::<f64>();
                println!(
                    "S5_C_EQUALIZED t={t:.1} cargo={ca:.1} ({cmin:.1}-{cmax:.1}) corridor={co:.1} kPa conserved={sum:.1} vs boot {:.1}",
                    boot.iter().sum::<f64>()
                );
                exit.write(AppExit::Success);
            }
        }
        // D - auto-door transient: demand opens the door, gas flows while
        // open, flow stops after the door closes again.
        "D" => {
            if !fired.contains(&"lower") && t >= 0.5 {
                fired.push("lower");
                for p in s5_region_tiles(&map, &comps, cargo) {
                    let removed = atmo.remove_fraction(p, 0.5);
                    astats.debug_removed(&removed);
                }
            }
            // Hold passage demand from t=1 to t=8 (old-seconds), like a crew
            // standing in the doorway.
            if (1.0..8.0).contains(&t) {
                demand.0.insert(TilePos::new(6, 6));
            }
            for (tag, at) in [("d1", 4.0), ("d2", 9.0), ("d3", 14.0)] {
                if !fired.contains(&tag) && t >= at {
                    fired.push(tag);
                    let (ca, _, _) =
                        s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, cargo));
                    let open = doors
                        .iter()
                        .find(|(p, _)| **p == TilePos::new(6, 6))
                        .map(|(_, d)| d.progress)
                        .unwrap_or(0.0);
                    println!("S5_D_{tag} t={t:.1} door_open={open:.2} cargo={ca:.1} kPa");
                }
            }
            if !fired.contains(&"done") && t >= 20.0 {
                fired.push("done");
                let (ca, _, _) =
                    s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, cargo));
                println!("S5_D_SETTLED t={t:.1} cargo={ca:.1} kPa (flow stopped after close)");
                exit.write(AppExit::Success);
            }
        }
        // E - composition mixing: O2-rich CARGO vs CO2-rich corridor (they
        // share door (6,6)) equalize their fractions, species conserved.
        "E" => {
            if !fired.contains(&"mix") && t >= 0.5 {
                fired.push("mix");
                let before = totals(&atmo);
                for p in s5_region_tiles(&map, &comps, cargo) {
                    atmo.set_mixture(
                        p,
                        crate::atmosphere::GasMixture {
                            mol: [70.0, 30.0, 0.0, 0.0],
                        },
                    );
                }
                for p in s5_region_tiles(&map, &comps, corridor) {
                    atmo.set_mixture(
                        p,
                        crate::atmosphere::GasMixture {
                            mol: [0.0, 70.0, 30.0, 0.0],
                        },
                    );
                }
                // Book the composition rewrite into the ledger so the
                // closed-system identity still audits.
                let after = totals(&atmo);
                for s in 0..4 {
                    astats.vented_mol[s] += before[s] - after[s];
                }
                set_door_mode(TilePos::new(6, 6), crate::airtight::DoorMode::HoldOpen);
            }
            if !fired.contains(&"done") && t >= 120.0 {
                fired.push("done");
                let fc = s5_region_fractions(&atmo, &s5_region_tiles(&map, &comps, cargo));
                let fk = s5_region_fractions(&atmo, &s5_region_tiles(&map, &comps, corridor));
                let now = totals(&atmo);
                let audited = now
                    .iter()
                    .zip(astats.vented_mol.iter())
                    .map(|(a, v)| a + v)
                    .sum::<f64>();
                println!(
                    "S5_E_MIXED t={t:.1} cargo O2={:.0}% CO2={:.0}% | corridor O2={:.0}% CO2={:.0}% | audit {:.1} vs boot {:.1}",
                    fc[0] * 100.0,
                    fc[2] * 100.0,
                    fk[0] * 100.0,
                    fk[2] * 100.0,
                    audited,
                    boot.iter().sum::<f64>(),
                );
                exit.write(AppExit::Success);
            }
        }
        // F - pollutant spreading: a corridor pocket stays out of sealed
        // rooms until a door opens.
        "F" => {
            if !fired.contains(&"inject") && t >= 0.5 {
                fired.push("inject");
                atmo.inject(
                    TilePos::new(10, 7),
                    &crate::atmosphere::GasMixture {
                        mol: [0.0, 0.0, 0.0, 8.0],
                    },
                );
            }
            for (tag, at) in [("f1", 15.0)] {
                if !fired.contains(&tag) && t >= at {
                    fired.push(tag);
                    let fp = s5_region_fractions(&atmo, &s5_region_tiles(&map, &comps, corridor));
                    let fc = s5_region_fractions(&atmo, &s5_region_tiles(&map, &comps, cargo));
                    println!(
                        "S5_F_{tag} t={t:.1} corridor pollutant={:.2}% cargo pollutant={:.3}% (doors closed)",
                        fp[3] * 100.0,
                        fc[3] * 100.0
                    );
                }
            }
            if !fired.contains(&"open") && t >= 16.0 {
                fired.push("open");
                set_door_mode(TilePos::new(6, 6), crate::airtight::DoorMode::HoldOpen);
            }
            if !fired.contains(&"done") && t >= 90.0 {
                fired.push("done");
                let fp = s5_region_fractions(&atmo, &s5_region_tiles(&map, &comps, corridor));
                let fc = s5_region_fractions(&atmo, &s5_region_tiles(&map, &comps, cargo));
                println!(
                    "S5_F_SPREAD t={t:.1} corridor pollutant={:.2}% cargo pollutant={:.3}% (door open)",
                    fp[3] * 100.0,
                    fc[3] * 100.0
                );
                exit.write(AppExit::Success);
            }
        }
        // G - decompression: breach drops pressure near the hole first, the
        // front moves inward, onboard gas falls while the vent ledger rises.
        "G" => {
            if !fired.contains(&"breach") && t >= 0.5 {
                fired.push("breach");
                crate::atmosphere::carve_breach(
                    &mut map,
                    &mut thermal,
                    &mut atmo,
                    TilePos::new(0, 7),
                );
            }
            for (tag, at) in [("g1", 3.0), ("g2", 6.0), ("g3", 10.0)] {
                if !fired.contains(&tag) && t >= at {
                    fired.push(tag);
                    let p1 = atmo.pressure_at(TilePos::new(1, 7), &thermal);
                    let p10 = atmo.pressure_at(TilePos::new(10, 7), &thermal);
                    let p25 = atmo.pressure_at(TilePos::new(25, 7), &thermal);
                    println!(
                        "S5_G_{tag} t={t:.1} x1={p1:.1} x10={p10:.1} x25={p25:.1} kPa vented={:.0} mol",
                        astats.vented_mol.iter().sum::<f64>()
                    );
                }
            }
            if !fired.contains(&"done") && t >= 12.0 {
                fired.push("done");
                let now = totals(&atmo);
                let vented = astats.vented_mol.iter().sum::<f64>();
                let heat = astats.vented_energy;
                println!(
                    "S5_G_VENTED t={t:.1} onboard={:.0} vented={:.0} audit={:.0} vs boot {:.0} | vented_heat={heat:.0}H",
                    now.iter().sum::<f64>(),
                    vented,
                    now.iter().sum::<f64>() + vented,
                    boot.iter().sum::<f64>(),
                );
                exit.write(AppExit::Success);
            }
        }
        // H - emergency isolation: Lock Closed before the breach keeps the
        // room's air while the corridor vents.
        "H" => {
            if !fired.contains(&"lock") && t >= 0.5 {
                fired.push("lock");
                set_door_mode(TilePos::new(16, 6), crate::airtight::DoorMode::LockClosed);
            }
            if !fired.contains(&"breach") && t >= 1.0 {
                fired.push("breach");
                crate::atmosphere::carve_breach(
                    &mut map,
                    &mut thermal,
                    &mut atmo,
                    TilePos::new(0, 7),
                );
            }
            if !fired.contains(&"done") && t >= 15.0 && !fired.contains(&"mid") {
                fired.push("mid");
                let (cr, _, _) =
                    s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, crew));
                let (co, _, _) =
                    s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, corridor));
                println!(
                    "S5_H_ISOLATION t={t:.1} crew_room={cr:.1} kPa corridor={co:.1} kPa (locked door saving the room)"
                );
            }
            if !fired.contains(&"done") && t >= 45.0 {
                fired.push("done");
                let (cr, _, _) =
                    s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, crew));
                let (co, _, _) =
                    s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, corridor));
                let vented = astats.vented_mol.iter().sum::<f64>();
                println!(
                    "S5_H_DEEP t={t:.1} crew_room={cr:.1} kPa corridor={co:.1} kPa vented={vented:.0} mol (locked door saved the room)"
                );
                exit.write(AppExit::Success);
            }
        }
        // I - re-open after isolation: equalization resumes.
        "I" => {
            if !fired.contains(&"breach") && t >= 0.5 {
                fired.push("breach");
                crate::atmosphere::carve_breach(
                    &mut map,
                    &mut thermal,
                    &mut atmo,
                    TilePos::new(0, 7),
                );
                set_door_mode(TilePos::new(16, 6), crate::airtight::DoorMode::LockClosed);
            }
            if !fired.contains(&"reopen") && t >= 15.0 {
                fired.push("reopen");
                set_door_mode(TilePos::new(16, 6), crate::airtight::DoorMode::HoldOpen);
            }
            if !fired.contains(&"done") && t >= 150.0 {
                fired.push("done");
                let (cr, crmin, crmax) =
                    s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, crew));
                let (co, _, _) =
                    s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, corridor));
                println!(
                    "S5_I_REOPEN t={t:.1} crew_room={cr:.1} ({crmin:.1}-{crmax:.1}) corridor={co:.1} kPa (re-equalizing)"
                );
                exit.write(AppExit::Success);
            }
        }
        // O - pause: state frozen while the renderer keeps running.
        "O" => {
            if !fired.contains(&"pause") && t >= 5.0 {
                fired.push("pause");
                speed.index = 0;
                *fired = fired.clone();
                fired.push("sample");
            }
            if fired.contains(&"pause") {
                let frames = fired.iter().filter(|&&f| f == "tick").count();
                if frames == 0 {
                    let (ca, _, _) = s5_region_pressure(
                        &atmo,
                        &thermal,
                        &s5_region_tiles(&map, &comps, corridor),
                    );
                    fired.push("frozen");
                    println!(
                        "S5_O_PAUSED t={t:.3} corridor={ca:.2} kPa active={}",
                        atmo.awake_count()
                    );
                    // stash the frozen sample inside the log line only
                }
                if frames < 90 {
                    fired.push("tick");
                } else if !fired.contains(&"done") {
                    fired.push("done");
                    let (ca, _, _) = s5_region_pressure(
                        &atmo,
                        &thermal,
                        &s5_region_tiles(&map, &comps, corridor),
                    );
                    println!(
                        "S5_O_FROZEN t={t:.3} corridor={ca:.2} kPa sim_now={:.3} (unchanged while paused)",
                        clock.now()
                    );
                    exit.write(AppExit::Success);
                }
            }
        }
        // P - speed equivalence: run to a fixed sim time at the forced speed
        // (SLICE0_SPEED) and dump state; compare across runs.
        "P" => {
            let idx: usize = std::env::var("SLICE0_SPEED")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            if !fired.contains(&"setup") {
                fired.push("setup");
                speed.index = idx;
            }
            if !fired.contains(&"lower") && t >= 0.5 {
                fired.push("lower");
                for p in s5_region_tiles(&map, &comps, cargo) {
                    let removed = atmo.remove_fraction(p, 0.5);
                    astats.debug_removed(&removed);
                }
                set_door_mode(TilePos::new(6, 6), crate::airtight::DoorMode::HoldOpen);
            }
            if !fired.contains(&"done") && clock.now() >= 3600.0 {
                fired.push("done");
                let (ca, _, _) =
                    s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, cargo));
                let (co, _, _) =
                    s5_region_pressure(&atmo, &thermal, &s5_region_tiles(&map, &comps, corridor));
                let now = totals(&atmo);
                println!(
                    "S5_P_EQ speed={idx} sim_t={:.0} cargo={ca:.2} corridor={co:.2} kPa o2={:.2} inert={:.2} co2={:.2} pol={:.2} active={}",
                    clock.now(),
                    now[0],
                    now[1],
                    now[2],
                    now[3],
                    atmo.awake_count(),
                );
                exit.write(AppExit::Success);
            }
        }
        // Q - sleep/wake: boot asleep, door-open wakes locally, quiet cells
        // sleep again.
        "Q" => {
            if !fired.contains(&"q0") {
                fired.push("q0");
                println!(
                    "S5_Q_BOOT t={t:.3} active={} (uniform sealed ship sleeps immediately)",
                    atmo.awake_count()
                );
            }
            if !fired.contains(&"open") && t >= 0.5 {
                fired.push("open");
                set_door_mode(TilePos::new(6, 6), crate::airtight::DoorMode::HoldOpen);
            }
            for (tag, at) in [("q1", 1.0), ("q2", 25.0)] {
                if !fired.contains(&tag) && t >= at {
                    fired.push(tag);
                    println!(
                        "S5_Q_{tag} t={t:.1} active={} (bounded workset: door convection + heated rooms)",
                        atmo.awake_count()
                    );
                }
            }
            // Close the door: the sealed compartment stops being updated by
            // the corridor and its cells fall back asleep.
            if !fired.contains(&"close") && t >= 26.0 {
                fired.push("close");
                set_door_mode(TilePos::new(6, 6), crate::airtight::DoorMode::LockClosed);
            }
            if !fired.contains(&"done") && t >= 90.0 {
                fired.push("done");
                println!(
                    "S5_Q_SETTLED t={t:.1} active={} (sealed CARGO asleep; heated FABRICATION workset remains)",
                    atmo.awake_count()
                );
                exit.write(AppExit::Success);
            }
        }
        _ => {}
    }
}

/// Developer atmosphere tools (`SLICE5_TOOLS=1`): hover a tile and press
/// - `F5` breach the hull wall under the cursor,
/// - `F6` drop the hovered compartment to ~60% pressure,
/// - `F7` restore the hovered compartment to standard atmosphere,
/// - `F8` inject CO₂ into the hovered compartment,
/// - `F9` inject pollutant into the hovered compartment.
///
/// Debug-only (never a player system); every mutation books into the vent
/// ledger so conservation audits stay meaningful.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn slice5_dev_tools(
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    mut map: ResMut<crate::map::ShipMap>,
    mut thermal: ResMut<crate::thermal::ThermalGrid>,
    mut atmo: ResMut<crate::atmosphere::AtmosphereGrid>,
    mut astats: ResMut<crate::atmosphere::AtmoStats>,
    comps: Res<crate::airtight::Compartments>,
    mut log: ResMut<EventLog>,
    mut overlay: ResMut<crate::OverlayMode>,
    mut pinned: Local<bool>,
) {
    // Boot-time overlay pin for scripted screenshots/playtests:
    // SLICE5_VIEW=power|thermal|coolant|compartments|atmosphere.
    if !*pinned {
        *pinned = true;
        if let Ok(v) = std::env::var("SLICE5_VIEW") {
            *overlay = match v.as_str() {
                "power" => crate::OverlayMode::Power,
                "thermal" => crate::OverlayMode::Thermal,
                "coolant" => crate::OverlayMode::Coolant,
                "compartments" => crate::OverlayMode::Compartments,
                "atmosphere" => crate::OverlayMode::Atmosphere,
                _ => crate::OverlayMode::Off,
            };
        }
    }
    if std::env::var("SLICE5_TOOLS").is_err() {
        return;
    }
    let Some(cursor) = windows.single().ok().and_then(|w| w.cursor_position()) else {
        return;
    };
    let Some((cam, cam_gt)) = camera.single().ok() else {
        return;
    };
    let Some(world) = cam.viewport_to_world_2d(cam_gt, cursor).ok() else {
        return;
    };
    let Some(p) = map.tile_at_world(world) else {
        return;
    };
    let now = crate::simtime::SimClock::real_secs_to_sim(0.0);
    let say = |log: &mut EventLog, text: String| {
        log.push(0.0, crate::log::LogKind::Info, text);
    };
    if keys.just_pressed(KeyCode::F5) {
        if crate::atmosphere::carve_breach(&mut map, &mut thermal, &mut atmo, p) {
            say(
                &mut log,
                format!("DEBUG: hull breach carved at ({},{})", p.x, p.y),
            );
        } else {
            say(
                &mut log,
                format!("DEBUG: ({},{}) is not a hull wall", p.x, p.y),
            );
        }
    }
    let region = s5_region_at(&comps, p);
    let region_ok = region != crate::airtight::NO_REGION;
    if keys.just_pressed(KeyCode::F6) && region_ok {
        let mut removed = crate::atmosphere::GasMixture::default();
        for q in s5_region_tiles(&map, &comps, region) {
            let r = atmo.remove_fraction(q, 0.4);
            for s in 0..4 {
                removed.mol[s] += r.mol[s];
            }
        }
        astats.debug_removed(&removed);
        say(
            &mut log,
            format!(
                "DEBUG: lowered pressure in compartment #{} by 40%",
                region + 1
            ),
        );
    }
    if keys.just_pressed(KeyCode::F7) && region_ok {
        let mut delta = crate::atmosphere::GasMixture::default();
        for q in s5_region_tiles(&map, &comps, region) {
            let old = atmo.mixture_at(q);
            atmo.set_mixture(q, crate::atmosphere::GasMixture::standard());
            for s in 0..4 {
                delta.mol[s] += old.mol[s] - crate::atmosphere::STANDARD_MIX[s];
            }
        }
        astats.debug_removed(&delta);
        say(
            &mut log,
            format!(
                "DEBUG: compartment #{} restored to standard atmosphere",
                region + 1
            ),
        );
    }
    let inject_region = |atmo: &mut crate::atmosphere::AtmosphereGrid,
                         astats: &mut crate::atmosphere::AtmoStats,
                         species: usize,
                         amount: f32|
     -> crate::atmosphere::GasMixture {
        let mut mix = crate::atmosphere::GasMixture::default();
        mix.mol[species] = amount;
        let tiles = s5_region_tiles(&map, &comps, region);
        let per = amount / tiles.len().max(1) as f32;
        let mut per_mix = crate::atmosphere::GasMixture::default();
        per_mix.mol[species] = per;
        for q in tiles {
            atmo.inject(q, &per_mix);
        }
        // Gas created by debug: book as negative vent so audits balance.
        let mut neg = crate::atmosphere::GasMixture::default();
        neg.mol[species] = -amount;
        astats.debug_removed(&neg);
        mix
    };
    if keys.just_pressed(KeyCode::F8) && region_ok {
        inject_region(&mut atmo, &mut astats, 2, 30.0);
        say(
            &mut log,
            format!("DEBUG: CO2 injected into compartment #{}", region + 1),
        );
    }
    if keys.just_pressed(KeyCode::F9) && region_ok {
        inject_region(&mut atmo, &mut astats, 3, 10.0);
        say(
            &mut log,
            format!("DEBUG: pollutant injected into compartment #{}", region + 1),
        );
    }
    let _ = now;
}

/// Dev hooks for door-art inspection on the full app (no scenario needed):
/// - `SLICE4_DEBUG_DOOR=x,y` spawns a finished door on that tile at boot,
///   bypassing build rules — e.g. straight into a vertical wall, so the Ew
///   leaf orientation can be exercised on the starter ship.
/// - `SLICE4_DOORPIN=x,y:progress[:mode]` pins a door's runtime state every
///   frame (progress 0..1; mode Auto/HoldOpen/LockClosed), so screenshots
///   capture exact mid-animation geometry instead of racing the door cycle.
///
/// Runs in `Set::Input`: after the FixedUpdate door logic, before the render
/// sync — what you see is exactly the pinned state.
fn slice4_dev_pins(
    mut commands: Commands,
    mut map: ResMut<crate::map::ShipMap>,
    mut doors: Query<(&TilePos, &mut crate::airtight::Door)>,
    mut spawned: Local<bool>,
) {
    if !*spawned {
        *spawned = true;
        if let Ok(spec) = std::env::var("SLICE4_DEBUG_DOOR") {
            let mut it = spec.split(',');
            if let (Some(x), Some(y)) = (
                it.next().and_then(|s| s.parse::<i32>().ok()),
                it.next().and_then(|s| s.parse::<i32>().ok()),
            ) {
                let pos = TilePos::new(x, y);
                // Floor works anywhere a real door could go; Wall/BuiltWall
                // lets the hook drop one straight into a vertical wall to
                // exercise the Ew orientation.
                let hostable = map.tile(pos).is_some_and(|t| {
                    t == crate::map::Tile::Floor
                        || t == crate::map::Tile::Wall
                        || t == crate::map::Tile::BuiltWall
                });
                if hostable {
                    let axis = crate::airtight::door_axis(&map, pos)
                        .unwrap_or(crate::airtight::DoorAxis::Ns);
                    map.set_tile(pos, crate::map::Tile::Door);
                    commands.spawn((
                        pos,
                        Building {
                            kind: BuildingKind::Door,
                            foot: Footprint::new(x, y, 1, 1),
                            demo_progress: 0.0,
                        },
                        crate::airtight::Door::new(axis),
                    ));
                } else {
                    println!("SLICE4_DEBUG_DOOR: ({x},{y}) is not a floor tile");
                }
            }
        }
    }
    let Ok(spec) = std::env::var("SLICE4_DOORPIN") else {
        return;
    };
    let mut parts = spec.split(':');
    let mut pos_it = parts.next().unwrap_or_default().split(',');
    let (Some(x), Some(y)) = (
        pos_it.next().and_then(|s| s.parse::<i32>().ok()),
        pos_it.next().and_then(|s| s.parse::<i32>().ok()),
    ) else {
        return;
    };
    let progress: f32 = parts
        .next()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);
    let mode = match parts.next().unwrap_or("HoldOpen") {
        "LockClosed" => crate::airtight::DoorMode::LockClosed,
        "Auto" => crate::airtight::DoorMode::Auto,
        _ => crate::airtight::DoorMode::HoldOpen,
    };
    for (p, mut door) in doors.iter_mut() {
        if p.x != x || p.y != y {
            continue;
        }
        door.mode = mode;
        door.progress = progress;
        door.phase = if progress >= 1.0 {
            crate::airtight::DoorPhase::Open
        } else if progress <= 0.0 {
            crate::airtight::DoorPhase::Closed
        } else {
            crate::airtight::DoorPhase::Opening
        };
        door.hold_until = f64::MAX / 2.0;
    }
}
