//! Headless integration tests for Slice 3: thermal grid + coolant loop on
//! the real starter ship. Runs the real system chain
//! (power → thermal air → coolant → thermal states) on a bare bevy_ecs World.

use bevy::ecs::schedule::Schedule;
use bevy::ecs::world::World;
use bevy::prelude::*;
use ship_alive::building::{Building, BuildingKind, Footprint};
use ship_alive::coolant::{PipeGrid, WaterGrid};
use ship_alive::map::{ShipMap, SpawnReq, TilePos, MAP_LAYOUT};
use ship_alive::power::{CableGrid, PowerRole, PowerStatus, FABRICATOR_DEMAND};
use ship_alive::storage::StorageCell;
use ship_alive::thermal::{ThermalBody, ThermalGrid, ThermalState};

fn setup_world() -> World {
    let mut world = World::new();
    let (map, spawns) = ShipMap::from_layout(&MAP_LAYOUT);
    let (w, h) = (map.width, map.height);
    world.insert_resource(map);
    world.insert_resource(ship_alive::log::EventLog::default());
    world.insert_resource(ship_alive::stats::Stats::default());
    let mut cables = CableGrid::new(w, h);
    let mut pipes = PipeGrid::new(w, h);
    let mut water = WaterGrid::new(w, h);
    let mut reservoir_tiles: Vec<TilePos> = Vec::new();
    for req in spawns {
        match req {
            SpawnReq::Crew { pos } => {
                world.spawn((
                    pos,
                    ship_alive::crew::Crew::new("C", Color::WHITE),
                    ship_alive::crew::CrewTask::default(),
                    ship_alive::crew::Movement::default(),
                ));
            }
            SpawnReq::Rack { pos, fill } => {
                let cell = match fill {
                    Some((kind, n)) => StorageCell::with_stock(kind, n),
                    None => StorageCell::default(),
                };
                world.spawn((
                    pos,
                    cell,
                    Footprint::new(pos.x, pos.y, 1, 1),
                    Building {
                        kind: BuildingKind::Rack,
                        foot: Footprint::new(pos.x, pos.y, 1, 1),
                        demo_progress: 0.0,
                    },
                ));
            }
            SpawnReq::Fabricator { pos } => {
                world.spawn((
                    pos,
                    Footprint::new(pos.x, pos.y, 2, 2),
                    Building {
                        kind: BuildingKind::Fabricator,
                        foot: Footprint::new(pos.x, pos.y, 2, 2),
                        demo_progress: 0.0,
                    },
                    ship_alive::production::Fabricator::default(),
                    PowerRole::consumer(FABRICATOR_DEMAND),
                    PowerStatus::default(),
                    ThermalBody::fabricator(),
                    ThermalState::default(),
                ));
            }
            SpawnReq::Reactor { pos } => {
                world.spawn((
                    pos,
                    Footprint::new(pos.x, pos.y, 2, 2),
                    Building {
                        kind: BuildingKind::Reactor,
                        foot: Footprint::new(pos.x, pos.y, 2, 2),
                        demo_progress: 0.0,
                    },
                    PowerRole::generator(),
                    PowerStatus::default(),
                    ThermalBody::reactor(),
                    ThermalState::default(),
                ));
            }
            SpawnReq::Cable { pos } => {
                cables.set(pos, true);
            }
            SpawnReq::Pipe { pos } => {
                pipes.set(pos, true);
            }
            SpawnReq::Pump { pos } => {
                world.spawn((
                    pos,
                    Footprint::new(pos.x, pos.y, 1, 1),
                    Building {
                        kind: BuildingKind::Pump,
                        foot: Footprint::new(pos.x, pos.y, 1, 1),
                        demo_progress: 0.0,
                    },
                    ship_alive::coolant::Pump,
                    PowerRole::consumer(ship_alive::coolant::PUMP_DEMAND),
                    PowerStatus::default(),
                    ThermalBody::pump(),
                    ThermalState::default(),
                ));
            }
            SpawnReq::Reservoir { pos } => {
                reservoir_tiles.push(pos);
                world.spawn((
                    pos,
                    Footprint::new(pos.x, pos.y, 1, 1),
                    Building {
                        kind: BuildingKind::Reservoir,
                        foot: Footprint::new(pos.x, pos.y, 1, 1),
                        demo_progress: 0.0,
                    },
                    ship_alive::coolant::Reservoir,
                    ThermalBody::passive(20.0),
                    ThermalState::default(),
                ));
            }
            SpawnReq::HeatExchanger { pos } => {
                world.spawn((
                    pos,
                    Footprint::new(pos.x, pos.y, 1, 1),
                    Building {
                        kind: BuildingKind::HeatExchanger,
                        foot: Footprint::new(pos.x, pos.y, 1, 1),
                        demo_progress: 0.0,
                    },
                    ship_alive::coolant::HeatExchanger,
                    ThermalBody::passive(8.0),
                    ThermalState::default(),
                ));
            }
            SpawnReq::Radiator { pos } => {
                world.spawn((
                    pos,
                    Footprint::new(pos.x, pos.y, 1, 1),
                    Building {
                        kind: BuildingKind::Radiator,
                        foot: Footprint::new(pos.x, pos.y, 1, 1),
                        demo_progress: 0.0,
                    },
                    ship_alive::coolant::Radiator { hull_ok: true },
                    ThermalBody::passive(10.0),
                    ThermalState::default(),
                ));
            }
            SpawnReq::Item { pos, kind } => {
                world.spawn((pos, ship_alive::items::Item { kind }));
            }
        }
    }
    // Boot state: 80%-filled loop at cabin temperature (mirrors setup.rs).
    for pos in pipes.iter_pipes() {
        water.fill(pos, 5.0, ship_alive::thermal::AMBIENT_START);
    }
    for pos in &reservoir_tiles {
        water.fill(*pos, 50.0, ship_alive::thermal::AMBIENT_START);
    }
    world.insert_resource(cables);
    world.insert_resource(pipes);
    world.insert_resource(water);
    {
        let map = world.resource::<ShipMap>();
        world.insert_resource(ThermalGrid::new(map));
    }
    world.insert_resource(ship_alive::coolant::CoolantState::default());
    world.insert_resource(ship_alive::coolant::CoolantStats::default());
    world.insert_resource(ship_alive::thermal::DeviceTiles::default());
    world.insert_resource(ship_alive::thermal::ThermalStats::default());
    world.insert_resource(ship_alive::power::PowerState::default());
    world.insert_resource(ship_alive::simtime::SimClock::default());
    world
}

fn thermal_schedule() -> Schedule {
    let mut schedule = Schedule::default();
    schedule.add_systems(
        (
            ship_alive::power::power_network_system,
            ship_alive::thermal::thermal_air_system,
            ship_alive::coolant::coolant_system,
            ship_alive::thermal::thermal_state_system,
        )
            .chain(),
    );
    schedule
}

fn step(world: &mut World, schedule: &mut Schedule, sim_secs: f64) {
    world
        .resource_mut::<ship_alive::simtime::SimClock>()
        .advance_sim(sim_secs);
    schedule.run(world);
}

fn reactor_foot(world: &mut World) -> Footprint {
    let mut rq = world.query::<(&Footprint, &Building)>();
    for (foot, b) in rq.iter(world) {
        if b.kind == BuildingKind::Reactor {
            return *foot;
        }
    }
    panic!("no reactor in world");
}

fn reactor_temp(world: &mut World) -> f32 {
    let foot = reactor_foot(world);
    world.resource::<ThermalGrid>().max_footprint_temp(&foot)
}

fn reactor_state(world: &mut World) -> ThermalState {
    let mut q = world.query::<(&Building, &ThermalState)>();
    for (b, s) in q.iter(world) {
        if b.kind == BuildingKind::Reactor {
            return *s;
        }
    }
    ThermalState::Normal
}

fn total_stored_heat(world: &World) -> f64 {
    let masses = &world.resource::<ship_alive::thermal::DeviceTiles>().mass;
    let grid = world.resource::<ThermalGrid>();
    let water = world.resource::<WaterGrid>();
    grid.total_heat(masses) + ship_alive::coolant::water_heat(water)
}

#[test]
fn starter_loop_is_stable_at_idle() {
    let mut world = setup_world();
    let mut schedule = thermal_schedule();
    // One warm-up step so DeviceTiles masses exist (the per-tile air
    // capacity changes once device mass lands — measure baselines after).
    step(&mut world, &mut schedule, 1.0);
    let water0 = world.resource::<WaterGrid>().total_water();
    let heat0 = total_stored_heat(&world);
    let stats0 = world
        .resource::<ship_alive::thermal::ThermalStats>()
        .clone();

    let mut max_reactor_t = f32::NEG_INFINITY;
    for minute in 0..90 {
        for _ in 0..60 {
            step(&mut world, &mut schedule, 1.0);
        }
        let t = reactor_temp(&mut world);
        max_reactor_t = max_reactor_t.max(t);
        if minute % 15 == 0 {
            println!(
                "t={}min reactor={t:.1}°C water_T={:.1} rad_total={:.0}",
                minute,
                water_avg_temp(&world),
                world
                    .resource::<ship_alive::thermal::ThermalStats>()
                    .radiated_total
            );
        }
    }
    println!("max reactor temp over 90 min: {max_reactor_t:.1}°C");
    assert!(
        max_reactor_t < 60.0,
        "starter ship must stay cool: {max_reactor_t}"
    );
    assert_eq!(reactor_state(&mut world), ThermalState::Normal);
    assert_eq!(
        world.resource::<WaterGrid>().total_water(),
        water0,
        "water is conserved"
    );

    // Conservation: stored heat grew by exactly injected minus radiated.
    let stats1 = world
        .resource::<ship_alive::thermal::ThermalStats>()
        .clone();
    let injected = stats1.injected_total - stats0.injected_total;
    let radiated = stats1.radiated_total - stats0.radiated_total;
    let stored = total_stored_heat(&world) - heat0;
    let net = injected - radiated;
    assert!(
        (stored - net).abs() < net.abs() * 1e-4 + 5.0,
        "conservation broken: stored {stored} vs in {injected} - out {radiated}"
    );
    assert!(radiated > 0.0, "the loop must actually dump heat");
}

#[test]
fn full_load_cooling_failure_reaches_crisis_and_recovers() {
    let mut world = setup_world();
    let mut schedule = thermal_schedule();

    // Force full reactor load: a big consumer on the cable network. It is
    // the youngest entity, so shedding would drop it first — size it to
    // exactly the leftover capacity (100 - pump 6 - map fab 20).
    let net_q = &mut world.query_filtered::<&TilePos, With<ship_alive::coolant::HeatExchanger>>();
    let hx_pos = net_q.iter(&world).next().copied().unwrap();
    world.spawn((
        TilePos::new(hx_pos.x, hx_pos.y),
        Footprint::new(hx_pos.x, hx_pos.y, 1, 1),
        PowerRole::consumer(74),
        PowerStatus::default(),
    ));

    // Warm up at full load with the loop running: must stay healthy.
    let inj0 = world
        .resource::<ship_alive::thermal::ThermalStats>()
        .injected_total;
    let mut inj_prev = inj0;
    for s in 1..=1800 {
        step(&mut world, &mut schedule, 1.0);
        if s % 300 == 0 {
            let inj = world
                .resource::<ship_alive::thermal::ThermalStats>()
                .injected_total;
            println!(
                "warmup t+{s}s reactor={:.1}°C heat_rate={:.1}/s nets={:?}",
                reactor_temp(&mut world),
                (inj - inj_prev) / 300.0,
                world.resource::<ship_alive::power::PowerState>().networks
            );
            inj_prev = inj;
        }
    }
    let t_full = reactor_temp(&mut world);
    println!("full-load steady reactor temp: {t_full:.1}°C");
    assert!(
        t_full < 70.0,
        "full load with intact loop must hold: {t_full}"
    );
    assert_eq!(reactor_state(&mut world), ThermalState::Normal);

    // Cut the ring between the pump and the radiators.
    {
        let mut pipes = world.remove_resource::<PipeGrid>().unwrap();
        let mut water = world.remove_resource::<WaterGrid>().unwrap();
        let mut cstats = world
            .remove_resource::<ship_alive::coolant::CoolantStats>()
            .unwrap();
        ship_alive::coolant::remove_pipe_preserving_water(
            &mut pipes,
            &mut water,
            &mut cstats,
            TilePos::new(16, 17),
        );
        world.insert_resource(pipes);
        world.insert_resource(water);
        world.insert_resource(cstats);
    }

    // Crisis develops within minutes: Overheat then Critical.
    let rad0 = world
        .resource::<ship_alive::thermal::ThermalStats>()
        .radiated_total;
    let mut rad_prev = rad0;
    let mut saw_overheat = false;
    let mut crit_at = None;
    for s in 0..3600 {
        step(&mut world, &mut schedule, 1.0);
        if (s + 1) % 300 == 0 {
            let rad = world
                .resource::<ship_alive::thermal::ThermalStats>()
                .radiated_total;
            println!(
                "post-cut t+{}s reactor={:.1}°C rad_rate={:.1}/s nets={:?} coolants={:?}",
                s + 1,
                reactor_temp(&mut world),
                (rad - rad_prev) / 300.0,
                world.resource::<ship_alive::power::PowerState>().networks,
                world
                    .resource::<ship_alive::coolant::CoolantState>()
                    .networks
            );
            rad_prev = rad;
        }
        match reactor_state(&mut world) {
            ThermalState::Overheat => saw_overheat = true,
            ThermalState::Critical if crit_at.is_none() => {
                crit_at = Some(s);
                println!(
                    "reactor CRITICAL at t+{s}s, temp {:.1}",
                    reactor_temp(&mut world)
                );
            }
            _ => {}
        }
    }
    assert!(saw_overheat, "cooling failure must derate the reactor");
    let crit_at = crit_at.expect("cooling failure must reach critical");
    assert!(
        crit_at < 3000,
        "crisis must arrive in minutes, got {crit_at}s"
    );

    // Emergency power keeps the pump alive (anti-deadlock).
    let nets = world
        .resource::<ship_alive::power::PowerState>()
        .networks
        .clone();
    let pump_powered = {
        let mut q = world.query_filtered::<&PowerStatus, With<ship_alive::coolant::Pump>>();
        q.iter(&world).next().map(|s| s.ok()).unwrap_or(false)
    };
    println!("nets at critical: {nets:?} pump powered: {pump_powered}");
    assert!(pump_powered, "emergency output must keep the pump running");

    // Repair: put the pipe back (with water) — recovery must follow.
    {
        let mut pipes = world.remove_resource::<PipeGrid>().unwrap();
        let mut water = world.remove_resource::<WaterGrid>().unwrap();
        pipes.set(TilePos::new(16, 17), true);
        water.fill(
            TilePos::new(16, 17),
            ship_alive::coolant::PIPE_TILE_CAP,
            ship_alive::thermal::AMBIENT_START,
        );
        world.insert_resource(pipes);
        world.insert_resource(water);
    }
    let mut recovered_at = None;
    for s in 0..3600 {
        step(&mut world, &mut schedule, 1.0);
        if (s + 1) % 300 == 0 {
            let mut rq = world.query::<(&Building, &PowerRole)>();
            let role = rq
                .iter(&world)
                .find(|(b, _)| b.kind == BuildingKind::Reactor)
                .map(|(_, r)| *r)
                .unwrap_or(PowerRole::consumer(0));
            println!(
                "post-repair t+{}s reactor={:.1}°C state={:?} role={role:?} coolants={:?}",
                s + 1,
                reactor_temp(&mut world),
                reactor_state(&mut world),
                world
                    .resource::<ship_alive::coolant::CoolantState>()
                    .networks
            );
        }
        if reactor_state(&mut world) == ThermalState::Normal && recovered_at.is_none() {
            recovered_at = Some(s);
            println!("recovered at t+{s}s, temp {:.1}", reactor_temp(&mut world));
        }
    }
    let rec = recovered_at.expect("cooling restored — reactor must recover");
    assert!(rec < 3000, "recovery should take minutes, got {rec}s");
}

fn water_avg_temp(world: &World) -> f32 {
    let water = world.resource::<WaterGrid>();
    let (mut a, mut b) = (0.0f32, 0.0f32);
    for (&amount, &t) in water.amount.iter().zip(water.temp.iter()) {
        a += amount;
        b += amount * t;
    }
    if a > 0.0 {
        b / a
    } else {
        0.0
    }
}

#[test]
fn thermal_state_hysteresis_no_flicker() {
    let mut world = setup_world();
    let mut schedule = thermal_schedule();
    // Warm one step so the footprint is queryable.
    step(&mut world, &mut schedule, 1.0);
    let _foot = reactor_foot(&mut world);

    // Heat the whole FABRICATION room: conduction flattens a lone hot
    // footprint within a single step, so pinning just the core would never
    // trip the state machine.
    let set_room = |world: &mut World, t: f32| {
        let mut grid = world.resource_mut::<ThermalGrid>();
        for y in 10..=17 {
            for x in 12..=21 {
                let p = TilePos::new(x, y);
                if grid.in_bounds(p) {
                    let i = grid.idx(p);
                    grid.amb[i] = t;
                    grid.wake(i);
                }
            }
        }
    };

    // Heat past the overheat threshold: state trips and HOLDS in the middle
    // of the hysteresis band (65..80 must not flip back to Normal).
    set_room(&mut world, 90.0);
    step(&mut world, &mut schedule, 1.0);
    assert_eq!(reactor_state(&mut world), ThermalState::Overheat);
    for _ in 0..5 {
        set_room(&mut world, 74.0);
        step(&mut world, &mut schedule, 1.0);
        assert_eq!(
            reactor_state(&mut world),
            ThermalState::Overheat,
            "must not flicker back inside the band"
        );
    }
    // Below the recovery threshold it clears.
    set_room(&mut world, 55.0);
    step(&mut world, &mut schedule, 1.0);
    assert_eq!(reactor_state(&mut world), ThermalState::Normal);

    // Critical trips at 120 and only clears below 100 (down to Overheat,
    // not straight to Normal).
    set_room(&mut world, 135.0);
    step(&mut world, &mut schedule, 1.0);
    assert_eq!(reactor_state(&mut world), ThermalState::Critical);
    set_room(&mut world, 108.0);
    step(&mut world, &mut schedule, 1.0);
    assert_eq!(
        reactor_state(&mut world),
        ThermalState::Critical,
        "holds above 100"
    );
    set_room(&mut world, 96.0);
    step(&mut world, &mut schedule, 1.0);
    assert_eq!(reactor_state(&mut world), ThermalState::Overheat);
}

#[test]
fn pause_invariance_and_speed_equivalence() {
    // The same total sim time must produce the same world state whether it
    // runs in one burst, in chunks with dt=0 pauses between, or interleaved
    // with idle frames — speeds never change world rules.
    // The app always steps at SIM_STEP=1; speeds only change how many steps
    // run per real second. So equivalence here = the same 1s steps with
    // dt=0 "paused frames" interleaved must not move the world.
    let run = |chunks: Vec<f64>| -> (f32, f32, f32) {
        let mut world = setup_world();
        let mut schedule = thermal_schedule();
        for c in chunks {
            step(&mut world, &mut schedule, c);
        }
        let t = reactor_temp(&mut world);
        let water_t = water_avg_temp(&world);
        let radiated = world
            .resource::<ship_alive::thermal::ThermalStats>()
            .radiated_total as f32;
        (t, water_t, radiated)
    };
    let plain = run(vec![1.0; 400]);
    let mut interleaved = Vec::new();
    for _ in 0..400 {
        interleaved.push(0.0);
        interleaved.push(1.0);
        interleaved.push(0.0);
    }
    let paused = run(interleaved);
    let close = |a: (f32, f32, f32), b: (f32, f32, f32)| {
        (a.0 - b.0).abs() < 0.5
            && (a.1 - b.1).abs() < 0.5
            && (a.2 - b.2).abs() < 0.02 * a.2.max(1.0)
    };
    assert!(
        close(plain, paused),
        "pauses must not matter: {plain:?} vs {paused:?}"
    );
}

#[test]
fn unpowered_pump_stagnates_loop() {
    let mut world = setup_world();
    let mut schedule = thermal_schedule();
    // Cut the pump's power by removing its power role status: disconnect the
    // reactor network's only other member... simplest: despawn the pump's
    // PowerRole (it becomes an unpowered consumer).
    {
        let mut q = world.query_filtered::<Entity, With<ship_alive::coolant::Pump>>();
        let pump = q.iter(&world).next().unwrap();
        world.entity_mut(pump).remove::<PowerRole>();
    }
    for _ in 0..120 {
        step(&mut world, &mut schedule, 1.0);
    }
    let nets = world
        .resource::<ship_alive::coolant::CoolantState>()
        .networks
        .clone();
    assert_eq!(nets.len(), 1);
    assert_eq!(nets[0].powered_pumps, 0, "pump must be unpowered");
    assert_eq!(nets[0].flow, 0.0, "no circulation without powered pump");
    assert_eq!(nets[0].status_label(), "Stagnant — pump unpowered");

    // Restore power: circulation resumes within a step.
    {
        let mut q = world.query_filtered::<Entity, With<ship_alive::coolant::Pump>>();
        let pump = q.iter(&world).next().unwrap();
        world
            .entity_mut(pump)
            .insert(PowerRole::consumer(ship_alive::coolant::PUMP_DEMAND));
    }
    step(&mut world, &mut schedule, 1.0);
    let nets = world
        .resource::<ship_alive::coolant::CoolantState>()
        .networks
        .clone();
    assert!(nets[0].powered_pumps >= 1);
    assert!(nets[0].flow > 0.0, "circulation resumes");
}

#[test]
fn network_split_and_merge_with_water_conservation() {
    let mut world = setup_world();
    let mut schedule = thermal_schedule();
    step(&mut world, &mut schedule, 1.0);
    let water0 = world.resource::<WaterGrid>().total_water();

    // Cut the ring: two networks, water preserved (neighbours have headroom).
    {
        let mut pipes = world.remove_resource::<PipeGrid>().unwrap();
        let mut water = world.remove_resource::<WaterGrid>().unwrap();
        let mut cstats = world
            .remove_resource::<ship_alive::coolant::CoolantStats>()
            .unwrap();
        // Simulate the fresh reservoir map the system maintains.
        cstats.reservoir_tiles = vec![pipes.idx(TilePos::new(21, 17))];
        ship_alive::coolant::remove_pipe_preserving_water(
            &mut pipes,
            &mut water,
            &mut cstats,
            TilePos::new(16, 17),
        );
        world.insert_resource(pipes);
        world.insert_resource(water);
        world.insert_resource(cstats);
    }
    step(&mut world, &mut schedule, 1.0);
    let nets = world
        .resource::<ship_alive::coolant::CoolantState>()
        .networks
        .clone();
    assert_eq!(nets.len(), 2, "cut ring splits into two networks: {nets:?}");
    assert_eq!(nets.iter().filter(|n| n.pumps > 0).count(), 1);
    assert_eq!(nets.iter().filter(|n| n.radiators > 0).count(), 1);
    assert!(
        (world.resource::<WaterGrid>().total_water() - water0).abs() < 1e-3,
        "water preserved across the split"
    );

    // Reconnect: one network again.
    {
        let mut pipes = world.remove_resource::<PipeGrid>().unwrap();
        pipes.set(TilePos::new(16, 17), true);
        world.insert_resource(pipes);
    }
    step(&mut world, &mut schedule, 1.0);
    let nets = world
        .resource::<ship_alive::coolant::CoolantState>()
        .networks
        .clone();
    assert_eq!(nets.len(), 1, "merged back: {nets:?}");
    assert_eq!(nets[0].tiles, 14);
}

#[test]
fn radiator_dump_is_capped_and_loop_transport_works() {
    let mut world = setup_world();
    let mut schedule = thermal_schedule();
    // Force full load so the loop moves serious heat.
    let mut q = world.query_filtered::<&TilePos, With<ship_alive::coolant::HeatExchanger>>();
    let hx = q.iter(&world).next().copied().unwrap();
    world.spawn((
        TilePos::new(hx.x, hx.y),
        Footprint::new(hx.x, hx.y, 1, 1),
        PowerRole::consumer(74),
        PowerStatus::default(),
    ));
    for _ in 0..1200 {
        step(&mut world, &mut schedule, 1.0);
    }
    let nets = world
        .resource::<ship_alive::coolant::CoolantState>()
        .networks
        .clone();
    assert_eq!(nets.len(), 1);
    let cap = 2.0 * ship_alive::coolant::RAD_MAX_DUMP;
    assert!(
        nets[0].dump_rate <= cap + 1.0,
        "dump respects the radiator cap: {} > {cap}",
        nets[0].dump_rate
    );
    assert!(nets[0].dump_rate > 200.0, "loop must transport real heat");
    assert!(
        reactor_state(&mut world) == ThermalState::Normal,
        "intact loop holds full load"
    );
}

#[test]
fn perf_128x128_synth_loop() {
    // A big synthetic ship: 128x128, one reactor + coolant loop in the
    // middle, everything else empty (sleeping) space. 1000 steps must stay
    // fast and the awake set must stay tiny.
    let mut world = World::new();
    let rows: Vec<String> = (0..128)
        .map(|y| {
            if y == 0 || y == 127 {
                "#".repeat(128)
            } else {
                format!("#{}#", ".".repeat(126))
            }
        })
        .collect();
    let layout: Vec<&str> = rows.iter().map(|s| s.as_str()).collect();
    let (map, _) = ShipMap::from_layout(&layout);
    let (w, h) = (map.width, map.height);
    world.insert_resource(map);
    world.insert_resource(ship_alive::log::EventLog::default());
    world.insert_resource(ship_alive::stats::Stats::default());
    world.insert_resource(CableGrid::new(w, h));
    world.insert_resource(PipeGrid::new(w, h));
    world.insert_resource(WaterGrid::new(w, h));
    world.insert_resource(ship_alive::coolant::CoolantState::default());
    world.insert_resource(ship_alive::coolant::CoolantStats::default());
    world.insert_resource(ship_alive::thermal::DeviceTiles::default());
    world.insert_resource(ship_alive::thermal::ThermalStats::default());
    world.insert_resource(ship_alive::power::PowerState::default());
    world.insert_resource(ship_alive::simtime::SimClock::default());

    // Reactor in the middle with a short cable stub.
    {
        let mut cables = world.remove_resource::<CableGrid>().unwrap();
        for &(x, y) in &[(62, 60), (63, 60), (64, 60)] {
            cables.set(TilePos::new(x, y), true);
        }
        world.insert_resource(cables);
    }
    world.spawn((
        TilePos::new(62, 61),
        Footprint::new(62, 61, 2, 2),
        Building {
            kind: BuildingKind::Reactor,
            foot: Footprint::new(62, 61, 2, 2),
            demo_progress: 0.0,
        },
        PowerRole::generator(),
        PowerStatus::default(),
        ThermalBody::reactor(),
        ThermalState::default(),
    ));

    // Coolant ring around the reactor: HX, pump, two radiators (hull_ok
    // false — this synthetic map's radiators are not against the border;
    // transport still exercises the rotation and exchange math).
    let ring: Vec<(i32, i32)> = (60..70)
        .map(|x| (x, 60))
        .chain((61..70).map(|y| (69, y)))
        .chain((60..69).rev().map(|x| (x, 69)))
        .chain((61..69).rev().map(|y| (y, 60)))
        .collect();
    {
        let mut pipes = world.remove_resource::<PipeGrid>().unwrap();
        let mut water = world.remove_resource::<WaterGrid>().unwrap();
        for &(x, y) in &ring {
            pipes.set(TilePos::new(x, y), true);
            water.fill(TilePos::new(x, y), 5.0, ship_alive::thermal::AMBIENT_START);
        }
        world.insert_resource(pipes);
        world.insert_resource(water);
    }
    {
        let map = world.resource::<ShipMap>();
        world.insert_resource(ThermalGrid::new(map));
    }
    for (x, y, kind) in [
        (60, 60, BuildingKind::HeatExchanger),
        (62, 60, BuildingKind::Pump),
        (65, 60, BuildingKind::Radiator),
        (68, 60, BuildingKind::Radiator),
    ] {
        let mut ec = world.spawn((
            TilePos::new(x, y),
            Footprint::new(x, y, 1, 1),
            Building {
                kind,
                foot: Footprint::new(x, y, 1, 1),
                demo_progress: 0.0,
            },
            ThermalBody::passive(8.0),
            ThermalState::default(),
        ));
        match kind {
            BuildingKind::Pump => {
                ec.insert(ship_alive::coolant::Pump)
                    .insert(PowerRole::consumer(ship_alive::coolant::PUMP_DEMAND))
                    .insert(PowerStatus::default());
            }
            BuildingKind::Radiator => {
                ec.insert(ship_alive::coolant::Radiator { hull_ok: false });
            }
            _ => {
                ec.insert(ship_alive::coolant::HeatExchanger);
            }
        }
    }

    let mut schedule = thermal_schedule();
    let t0 = std::time::Instant::now();
    for _ in 0..1000 {
        step(&mut world, &mut schedule, 1.0);
    }
    let dt = t0.elapsed().as_secs_f32();
    let active = world
        .resource::<ship_alive::thermal::ThermalStats>()
        .active_tiles;
    println!(
        "PERF 128x128: 1000 steps in {dt:.2}s ({:.0} steps/s), awake tiles {}/16384",
        1000.0 / dt,
        active
    );
    assert!(dt < 5.0, "1000 steps must stay fast, took {dt:.2}s");
    assert!(
        active < 14000,
        "awake set must stay bounded: {active} awake"
    );
}
