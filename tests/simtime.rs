//! Integration tests for the unified simulation-time architecture:
//! pause/resume/last-speed memory, scale & FPS equivalence of real gameplay,
//! hitch recovery, and long-run precision through the actual systems.

use bevy::ecs::schedule::Schedule;
use bevy::ecs::world::World;
use bevy::prelude::*;
use ship_alive::crew::{Crew, CrewTask, Movement};
use ship_alive::jobs::{self, Action};
use ship_alive::log::EventLog;
use ship_alive::map::{ShipMap, TilePos};
use ship_alive::simtime::{self, SimClock};

const LAYOUT: [&str; 5] = ["#######", "#C..S.#", "#.....#", "#.....#", "#######"];

/// Headless frame runner: each "frame" offers a real dt to the pump, then
/// runs the gameplay schedule once per drained fixed step — exactly the
/// app's PreUpdate pump / FixedUpdate step split.
struct Frames {
    world: World,
    schedule: Schedule,
}

fn frames() -> Frames {
    let mut world = World::new();
    let (map, _) = ShipMap::from_layout(&LAYOUT);
    let (w, h) = (map.width, map.height);
    world.insert_resource(map);
    world.insert_resource(EventLog::default());
    world.insert_resource(ship_alive::stats::Stats::default());
    world.insert_resource(ship_alive::power::CableGrid::new(w, h));
    world.insert_resource(ship_alive::coolant::PipeGrid::new(w, h));
    world.insert_resource(ship_alive::coolant::WaterGrid::new(w, h));
    let thermal_grid = {
        let map = world.resource::<ship_alive::map::ShipMap>();
        ship_alive::thermal::ThermalGrid::new(map)
    };
    world.insert_resource(thermal_grid);
    world.insert_resource(ship_alive::coolant::CoolantStats::default());
    world.insert_resource(ship_alive::power::PowerState::default());
    world.insert_resource(SimClock::default());
    world.init_resource::<Events<Action>>();
    let mut schedule = Schedule::default();
    schedule.add_systems(
        (
            jobs::actions_system,
            jobs::crew_task_system,
            jobs::crew_scan_system,
            ship_alive::movement::movement_system,
        )
            .chain(),
    );
    Frames { world, schedule }
}

impl Frames {
    /// One "render frame": offer real dt at the given player scale, then run
    /// the gameplay schedule once per fixed step drained.
    fn frame(&mut self, real_dt: f64, scale: f64) -> u32 {
        self.world
            .resource_mut::<SimClock>()
            .offer_real_delta(real_dt, scale);
        let mut steps = 0;
        loop {
            let more = self.world.resource_mut::<SimClock>().begin_fixed_step();
            if !more {
                break;
            }
            self.world.resource_mut::<Events<Action>>().update();
            self.schedule.run(&mut self.world);
            steps += 1;
        }
        steps
    }
}

// E — Space pause/resume remembers the last non-paused speed.
#[test]
fn pause_resumes_last_speed() {
    let mut world = World::new();
    world.insert_resource(ship_alive::time_ctrl::GameSpeed::default());
    world.insert_resource(EventLog::default());
    world.insert_resource(SimClock::default());
    world.init_resource::<Events<Action>>();
    let mut schedule = Schedule::default();
    schedule.add_systems((ship_alive::time_ctrl::speed_action_system,));

    let send = |w: &mut World, a: Action| w.resource_mut::<Events<Action>>().send(a);
    let run = |w: &mut World, s: &mut Schedule| {
        w.resource_mut::<Events<Action>>().update();
        s.run(w);
    };

    send(&mut world, Action::SetSpeed { index: 3 });
    run(&mut world, &mut schedule);
    assert_eq!(
        world.resource::<ship_alive::time_ctrl::GameSpeed>().index,
        3
    );
    send(&mut world, Action::TogglePause);
    run(&mut world, &mut schedule);
    assert_eq!(
        world.resource::<ship_alive::time_ctrl::GameSpeed>().index,
        0
    );
    send(&mut world, Action::TogglePause);
    run(&mut world, &mut schedule);
    assert_eq!(
        world.resource::<ship_alive::time_ctrl::GameSpeed>().index,
        3,
        "Space must resume the previous non-paused speed"
    );

    send(&mut world, Action::SetSpeed { index: 2 });
    run(&mut world, &mut schedule);
    send(&mut world, Action::TogglePause);
    run(&mut world, &mut schedule);
    send(&mut world, Action::TogglePause);
    run(&mut world, &mut schedule);
    assert_eq!(
        world.resource::<ship_alive::time_ctrl::GameSpeed>().index,
        2
    );
}

// F/G — scale equivalence on real gameplay: same sim target via different
// player scales must land on identical state.
#[test]
fn scale_equivalence_on_real_gameplay() {
    let run = |scale: f64, total_real: f64| -> (f64, TilePos) {
        let mut f = frames();
        let crew = f
            .world
            .spawn((
                TilePos::new(1, 1),
                Crew::new("A", Color::WHITE),
                CrewTask::default(),
                Movement {
                    path: (2..=5).map(|x| TilePos::new(x, 1)).collect(),
                    ..default()
                },
            ))
            .id();
        // Fixed slice count so both scales receive identical total real
        // time without 1/60 accumulation drift.
        let n = 120usize;
        for _ in 0..n {
            f.frame(total_real / n as f64, scale);
        }
        let pos = *f.world.get::<TilePos>(crew).unwrap();
        (f.world.resource::<SimClock>().now(), pos)
    };
    // 2 real seconds at 1x == 1 real second at 2x (both = 120 sim s).
    let (t1, p1) = run(1.0, 2.0);
    let (t2, p2) = run(2.0, 1.0);
    assert_eq!(t1, t2);
    assert_eq!(p1, p2, "same sim time must give the same gameplay state");
    assert_eq!(t1, 120.0);
}

// H/I — FPS independence + irregular frame cadence.
#[test]
fn fps_and_cadence_independence() {
    let run = |chunks: &[f64]| -> (f64, TilePos, f32) {
        let mut f = frames();
        let crew = f
            .world
            .spawn((
                TilePos::new(1, 1),
                Crew::new("A", Color::WHITE),
                CrewTask::default(),
                Movement {
                    path: (2..=5).map(|x| TilePos::new(x, 1)).collect(),
                    ..default()
                },
            ))
            .id();
        for dt in chunks {
            f.frame(*dt, 1.0);
        }
        let progress = f.world.get::<Movement>(crew).unwrap().progress;
        let pos = *f.world.get::<TilePos>(crew).unwrap();
        (f.world.resource::<SimClock>().now(), pos, progress)
    };

    // Same total real time (2 s) delivered at different cadences.
    const TOTAL: f64 = 2.0;
    let even: Vec<f64> = vec![1.0 / 60.0; 120];
    let slow: Vec<f64> = vec![1.0 / 30.0; 60];
    let pattern = [0.016, 0.016, 0.033, 0.008, 0.05, 0.024];
    let pattern_sum: f64 = pattern.iter().sum();
    let reps = (TOTAL / pattern_sum).floor() as usize;
    let mut irregular: Vec<f64> = pattern
        .iter()
        .cycle()
        .take(reps * pattern.len())
        .copied()
        .collect();
    irregular.push(TOTAL - reps as f64 * pattern_sum); // remainder

    let a = run(&even);
    let b = run(&slow);
    let c = run(&irregular);
    assert_eq!(a.0, b.0, "even vs half-rate must match exactly");
    // Irregular cadence may trail by at most one fixed step (sub-step
    // remainder sits in the accumulator, not the clock — by design).
    assert!(
        (a.0 - c.0).abs() <= simtime::SIM_STEP,
        "irregular cadence drifted: {a:?} vs {c:?}"
    );
    assert_eq!(a.1, b.1);
    assert_eq!(a.1, c.1, "crew state must match at equal sim time");
    assert!((a.2 - b.2).abs() < 1e-4);
    assert!((a.2 - c.2).abs() < 1e-3);
}

// D — pause freezes clock and gameplay; pause+resume == no pause.
#[test]
fn pause_freezes_gameplay_and_clock() {
    let mut f = frames();
    let crew = f
        .world
        .spawn((
            TilePos::new(1, 1),
            Crew::new("A", Color::WHITE),
            CrewTask::default(),
            Movement {
                path: (2..=5).map(|x| TilePos::new(x, 1)).collect(),
                ..default()
            },
        ))
        .id();
    f.frame(1.0, 1.0);
    let (t, pos) = {
        let t = f.world.resource::<SimClock>().now();
        (t, *f.world.get::<TilePos>(crew).unwrap())
    };
    for _ in 0..30 {
        f.frame(1.0 / 60.0, 0.0); // paused
    }
    assert_eq!(f.world.resource::<SimClock>().now(), t);
    assert_eq!(*f.world.get::<TilePos>(crew).unwrap(), pos);
    // Pause+resume to a target equals never pausing.
    f.frame(1.0 / 60.0, 1.0);
    let t_after = f.world.resource::<SimClock>().now();
    let mut g = frames();
    g.frame(t_after / simtime::BASE_SIM_RATE, 1.0);
    assert_eq!(g.world.resource::<SimClock>().now(), t_after);
}

// O — long frame (hitch): bounded steps, clock == processed, backlog drains.
#[test]
fn hitch_recovers_via_backlog() {
    let mut f = frames();
    // A 250 ms frame at 1x offers 15 sim s = 15 steps in one frame.
    let steps = f.frame(0.250, 1.0);
    assert_eq!(steps, 15, "one 250ms hitch drains as 15 fixed steps");
    assert_eq!(f.world.resource::<SimClock>().now(), 15.0);
    assert!(f.world.resource::<SimClock>().backlog_secs() < 1.0);
    let mut g = frames();
    let steps2 = g.frame(0.500, 1.0);
    assert!(steps2 > 0 && steps2 <= 30);
    assert!(g.world.resource::<SimClock>().backlog_secs() < 1.0);
}

// N — rapid speed switching never double-ticks or rewinds.
#[test]
fn rapid_speed_switching_is_stable() {
    let mut f = frames();
    let scales = [1.0, 4.0, 0.0, 2.0, 4.0, 1.0, 0.0, 1.0];
    let mut steps = 0u64;
    for s in scales {
        steps += f.frame(1.0 / 60.0, s) as u64;
    }
    assert_eq!(
        f.world.resource::<SimClock>().now(),
        steps as f64,
        "clock must equal exactly the executed steps"
    );
}

// P — long-run precision through the full pipeline.
#[test]
fn long_run_exact_seconds() {
    let mut f = frames();
    let chunk: f64 = 60.0; // one real minute per call = 3600 sim s
    let mut offered = 0.0f64;
    let target = 100.0; // real seconds at 1x
    while offered < target {
        let dt = chunk.min(target - offered);
        offered += dt;
        f.frame(dt, 1.0);
    }
    let now = f.world.resource::<SimClock>().now();
    assert_eq!(now, 6000.0);
    assert_eq!(simtime::format_sim_stamp(now), "T+001:40:00");
    assert_eq!(now % 1.0, 0.0, "whole seconds stay exact after long runs");
}

// B — the 24 h boundary through the pump path (no day wrap).
#[test]
fn twenty_four_hour_boundary() {
    let mut c = SimClock::default();
    c.offer_real_delta(1440.0, 1.0); // 24 h of sim at 1x
    while c.begin_fixed_step() {}
    assert_eq!(simtime::format_sim_stamp(c.now()), "T+024:00:00");
    c.offer_real_delta(1.0 / 60.0, 1.0);
    while c.begin_fixed_step() {}
    assert_eq!(
        simtime::format_sim_stamp(c.now()),
        "T+024:00:01",
        "no day wrap"
    );
}
