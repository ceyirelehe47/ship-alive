//! Automated acceptance-scenario driver (dev tool).
//!
//! `SLICE0_SCENARIO=A|B|C|D|E|F cargo run` scripts player actions on a
//! schedule (mark items, set speed, delete a target, …), then prints a world
//! state summary and exits. Used to smoke-test the acceptance scenarios from
//! section 14 of the design brief without manual play.

use crate::crew::{Crew, CrewTask};
use crate::items::{Item, ItemKind, MarkedForHaul, ReservedBy};
use crate::jobs::Action;
use crate::log::EventLog;
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

fn dump_and_exit(
    ctx: &str,
    items: &Query<(Entity, &Item), With<MarkedForHaul>>,
    crews: &Query<&CrewTask, With<Crew>>,
    racks: &Query<&StorageCell>,
    log: &EventLog,
    exit: &mut EventWriter<AppExit>,
) {
    let stored: u32 = racks.iter().map(|s| s.stored()).sum();
    let free: u32 = racks.iter().map(|s| s.free()).sum();
    let crew: Vec<String> = crews
        .iter()
        .map(|t| match t {
            CrewTask::Idle(c) => c.label(),
            CrewTask::Haul(_) => "working".to_string(),
        })
        .collect();
    println!(
        "SCENARIO_RESULT scenario={ctx} marked_left={} stored={stored} free={free} crew={crew:?}",
        items.iter().count()
    );
    println!("LOG_TAIL_BEGIN");
    for e in log.entries.iter().rev().take(14).collect::<Vec<_>>().iter().rev() {
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
    crews: Query<&CrewTask, With<Crew>>,
    racks: Query<&StorageCell>,
    log: Res<EventLog>,
    mut exit: EventWriter<AppExit>,
    mut fired: Local<Vec<&'static str>>,
    mut last_trace: Local<f64>,
    trace_crews: Query<(&Crew, &CrewTask, &crate::map::TilePos, &crate::crew::Movement)>,
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
                dump_and_exit("A", &items, &crews, &racks, &log, &mut exit);
            }
        }
        // B: storage full — spawn items beyond capacity, expect idle + no thrash.
        "B" => {
            fire("mark", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            fire("spawn", 0.5, t, &mut fired, &mut actions, |a| {
                for _ in 0..14 {
                    let _ = a.write(Action::SpawnItem { kind: ItemKind::Crate });
                }
            });
            fire("speed", 0.7, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"done") && t >= 160.0 {
                fired.push("done");
                dump_and_exit("B", &items, &crews, &racks, &log, &mut exit);
            }
        }
        // C: unreachable target — the sealed pocket item is marked with the rest.
        "C" => {
            fire("mark", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            if !fired.contains(&"done") && t >= 15.0 {
                fired.push("done");
                dump_and_exit("C", &items, &crews, &racks, &log, &mut exit);
            }
        }
        // D: competition — all four crew wake at once against few items.
        "D" => {
            fire("mark", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            if !fired.contains(&"done") && t >= 5.0 {
                fired.push("done");
                dump_and_exit("D", &items, &crews, &racks, &log, &mut exit);
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
                dump_and_exit("E", &items, &crews, &racks, &log, &mut exit);
            }
        }
        // F: max speed under load — everything marked at 4x plus spawned items.
        "F" => {
            fire("mark", 0.3, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::MarkAll);
            });
            fire("spawn", 0.5, t, &mut fired, &mut actions, |a| {
                for _ in 0..10 {
                    let _ = a.write(Action::SpawnItem { kind: ItemKind::Ore });
                }
            });
            fire("speed", 0.8, t, &mut fired, &mut actions, |a| {
                let _ = a.write(Action::SetSpeed { index: 3 });
            });
            if !fired.contains(&"done") && t >= 150.0 || (t >= 30.0 && items.iter().count() == 0) {
                fired.push("done");
                dump_and_exit("F", &items, &crews, &racks, &log, &mut exit);
            }
        }
        _ => {}
    }
}
