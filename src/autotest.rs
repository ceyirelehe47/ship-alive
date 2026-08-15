//! Automated acceptance-scenario driver (dev tool).
//!
//! `SLICE0_SCENARIO=A..L cargo run` scripts player actions on a schedule
//! (mark items, place blueprints, set orders and priorities, …), then prints
//! a world state summary and exits. Used to smoke-test the acceptance
//! scenarios from the design briefs without manual play.

use crate::building::{Blueprint, Building, BuildingKind};
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
        app.add_systems(Update, scenario_driver.in_set(crate::Set::Input));
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
    time: Res<Time<Virtual>>,
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
    mut rework: Local<(u32, u32)>,
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
    let t = time.elapsed().as_secs_f64();

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
                    pos: TilePos::new(20, 11),
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
                let _ = a.write(Action::PlaceBlueprint {
                    kind: BuildingKind::Door,
                    pos: TilePos::new(19, 11),
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
                .any(|(_, p, b)| b.kind == BuildingKind::Door && p.x == 19 && p.y == 11);
            if !fired.contains(&"done") && ((t >= 20.0 && rebuilt && door_built) || t >= 240.0) {
                fired.push("done");
                println!(
                    "P1_RESULT rack_rebuilt={rebuilt} wall_canceled={} door_built={}",
                    fired.contains(&"p1_cancel"),
                    buildings
                        .iter()
                        .any(|(_, p, b)| b.kind == BuildingKind::Door && p.x == 19 && p.y == 11),
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
                    "M_IMPROVED t_next_5_parts={} haul_dist_since_rework={} hauls_since_rework={}",
                    t,
                    stats.haul_distance.saturating_sub(rework.0),
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
