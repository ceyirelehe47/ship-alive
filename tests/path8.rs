//! Headless tests for the 8-way pathfinding upgrade: movement timing
//! (diagonal gets no speed boost), mixed-step stability across game speeds,
//! dynamic wall regression, and soft avoidance under 8-way traffic.

use bevy::ecs::schedule::Schedule;
use bevy::ecs::world::World;
use bevy::prelude::*;
use ship_alive::crew::{Crew, CrewTask, Movement};
use ship_alive::log::EventLog;
use ship_alive::map::{ShipMap, Tile, TilePos};
use ship_alive::path;

fn crew(name: &str, tiles_per_real_sec: f32) -> Crew {
    let mut c = Crew::new(name, Color::WHITE);
    // Crew::new already expresses the default 3 tiles/real-s in sim units;
    // tests specify real-second speeds for readability.
    c.speed = tiles_per_real_sec / ship_alive::simtime::BASE_SIM_RATE as f32;
    c
}

/// Movement-only harness: map + crew, no job systems.
fn movement_world(rows: &[&str]) -> World {
    let mut world = World::new();
    let (map, _) = ShipMap::from_layout(rows);
    world.insert_resource(map);
    world.insert_resource(EventLog::default());
    world.insert_resource(ship_alive::simtime::SimClock::default());
    world
}

fn run_until_arrival(
    world: &mut World,
    schedule: &mut Schedule,
    mover: Entity,
    dt: f32,
    max_secs: f32,
) -> f32 {
    let mut t = 0.0;
    while t < max_secs {
        world
            .resource_mut::<ship_alive::simtime::SimClock>()
            .advance_sim(dt as f64 * ship_alive::simtime::BASE_SIM_RATE);
        schedule.run(world);
        t += dt;
        if world.get::<Movement>(mover).unwrap().path.is_empty() {
            return t;
        }
    }
    panic!("did not arrive within {max_secs}s");
}

// H — diagonal speed must equal cardinal speed in world space.
#[test]
fn diagonal_has_no_speed_boost() {
    // Two legs run in SEPARATE worlds: movement advances every crew each
    // frame, so a co-spawned walker would finish during the other's timing.
    let field = &[
        "..............",
        "..............",
        "..............",
        "..............",
        "..............",
        "..............",
        "..............",
        "..............",
        "..............",
        "..............",
        "..............",
        "..............",
        "..............",
        "..............",
    ];

    let (t_card, t_diag);
    {
        let mut world = movement_world(field);
        let mut schedule = Schedule::default();
        schedule.add_systems((ship_alive::movement::movement_system,));
        // 10 cardinal steps east = world distance 10.
        let card = world
            .spawn((
                TilePos::new(1, 1),
                crew("card", 3.0),
                CrewTask::default(),
                Movement {
                    path: (2..=11).map(|x| TilePos::new(x, 1)).collect(),
                    ..default()
                },
            ))
            .id();
        t_card = run_until_arrival(&mut world, &mut schedule, card, 1.0 / 60.0, 30.0);
    }
    {
        let mut world = movement_world(field);
        let mut schedule = Schedule::default();
        schedule.add_systems((ship_alive::movement::movement_system,));
        // 10 diagonal steps south-east = world distance 10·√2.
        let diag = world
            .spawn((
                TilePos::new(1, 2),
                crew("diag", 3.0),
                CrewTask::default(),
                Movement {
                    path: (1..=10).map(|i| TilePos::new(1 + i, 2 + i)).collect(),
                    ..default()
                },
            ))
            .id();
        t_diag = run_until_arrival(&mut world, &mut schedule, diag, 1.0 / 60.0, 30.0);
    }

    // World speeds: 10/t_card and 10√2/t_diag must both be ≈ 3 tiles/s.
    let speed_card = 10.0 / t_card;
    let speed_diag = 10.0 * std::f32::consts::SQRT_2 / t_diag;
    assert!(
        (speed_card - 3.0).abs() < 0.10,
        "cardinal speed drifted: {speed_card}"
    );
    assert!(
        (speed_diag - 3.0).abs() < 0.10,
        "diagonal world speed drifted (speed boost?): {speed_diag}"
    );
    // And the diagonal leg genuinely takes ~√2 longer for equal step count.
    assert!((t_diag / t_card - std::f32::consts::SQRT_2).abs() < 0.05);
}

// I — mixed cardinal/diagonal steps stay stable at 1x/2x/4x tick rates.
#[test]
fn mixed_steps_stable_across_speeds() {
    let rows = &[
        "..............",
        "..............",
        "..............",
        "..............",
        "..............",
        "..............",
    ];
    let mut expected_pos = None;
    for (dt, label) in [(1.0 / 60.0, "1x"), (2.0 / 60.0, "2x"), (4.0 / 60.0, "4x")] {
        let mut world = movement_world(rows);
        let mut schedule = Schedule::default();
        schedule.add_systems((ship_alive::movement::movement_system,));
        // cardinal → diagonal → cardinal → diagonal → cardinal
        let path = vec![
            TilePos::new(2, 1),
            TilePos::new(3, 2),
            TilePos::new(4, 2),
            TilePos::new(5, 3),
            TilePos::new(6, 3),
        ];
        let total = path::path_length(Some(TilePos::new(1, 1)), &path);
        let m = world
            .spawn((
                TilePos::new(1, 1),
                crew("m", 3.0),
                CrewTask::default(),
                Movement { path, ..default() },
            ))
            .id();
        let t = run_until_arrival(&mut world, &mut schedule, m, dt, 30.0);
        let pos = *world.get::<TilePos>(m).unwrap();
        assert_eq!(pos, TilePos::new(6, 3), "{label}: wrong arrival tile");
        // Arrival time must match the geometric length regardless of tick.
        assert!(
            (t - total / 3.0).abs() < 0.2,
            "{label}: timing drifted: took {t:.3}, expected {:.3}",
            total / 3.0
        );
        expected_pos = Some(pos);
    }
    assert_eq!(expected_pos, Some(TilePos::new(6, 3)));
}

// J2 — dynamic wall: 8-way must not cut the new corner; removal restores it.
#[test]
fn dynamic_wall_blocks_diagonal_and_removal_restores() {
    // Open room; build a wall that makes the (1,1)->(3,3) diagonal illegal.
    let rows = &[".....", ".....", ".....", ".....", "....."];
    let mut world = movement_world(rows);
    world
        .resource_mut::<ShipMap>()
        .set_tile(TilePos::new(2, 1), Tile::BuiltWall);

    let p = path::find_path(
        world.resource::<ShipMap>(),
        TilePos::new(1, 1),
        TilePos::new(3, 3),
        |_| false,
    )
    .expect("route around exists");
    // The (1,1)->(2,2) diagonal has walled side cell (2,1) and must never be
    // used; the legal detour (around the wall tile) may even cost less than
    // the pure diagonal pair, so assert per-step legality instead.
    let mut prev = TilePos::new(1, 1);
    for t in &p {
        let diagonal = prev.x != t.x && prev.y != t.y;
        if diagonal {
            let (side_a, side_b) = (TilePos::new(t.x, prev.y), TilePos::new(prev.x, t.y));
            assert!(
                world.resource::<ShipMap>().is_walkable(side_a)
                    && world.resource::<ShipMap>().is_walkable(side_b),
                "cut the walled corner at {prev:?}->{t:?}"
            );
        }
        prev = *t;
    }
    assert_eq!(*p.last().unwrap(), TilePos::new(3, 3));

    // Tear the wall down: the pure diagonal is back.
    world
        .resource_mut::<ShipMap>()
        .set_tile(TilePos::new(2, 1), Tile::Floor);
    let p2 = path::find_path(
        world.resource::<ShipMap>(),
        TilePos::new(1, 1),
        TilePos::new(3, 3),
        |_| false,
    )
    .unwrap();
    // Two pure diagonal steps, nothing longer.
    assert_eq!(
        path::path_cost(Some(TilePos::new(1, 1)), &p2),
        2 * path::COST_DIAGONAL
    );
    assert_eq!(p2.len(), 2);
}

// K — soft avoidance under 8-way: head-on in a corridor resolves; diagonal
// merge does not deadlock; nobody stalls forever.
#[test]
fn soft_avoidance_headon_and_diagonal_merge() {
    let rows = &["#######", "#.....#", "#.....#", "#.....#", "#######"];
    let mut world = movement_world(rows);
    let mut schedule = Schedule::default();
    schedule.add_systems((ship_alive::movement::movement_system,));

    // Head-on: A walks east, B walks west along row 2.
    let a = world
        .spawn((
            TilePos::new(1, 2),
            crew("A", 3.0),
            CrewTask::default(),
            Movement {
                path: (2..=5).map(|x| TilePos::new(x, 2)).collect(),
                ..default()
            },
        ))
        .id();
    let b = world
        .spawn((
            TilePos::new(5, 2),
            crew("B", 3.0),
            CrewTask::default(),
            Movement {
                path: (1..=4).rev().map(|x| TilePos::new(x, 2)).collect(),
                ..default()
            },
        ))
        .id();
    let mut t = 0.0;
    while t < 30.0 {
        world
            .resource_mut::<ship_alive::simtime::SimClock>()
            .advance_sim(1.0 / 60.0 * ship_alive::simtime::BASE_SIM_RATE);
        schedule.run(&mut world);
        t += 1.0 / 60.0;
        if world.get::<Movement>(a).unwrap().path.is_empty()
            && world.get::<Movement>(b).unwrap().path.is_empty()
        {
            break;
        }
    }
    assert!(
        world.get::<Movement>(a).unwrap().path.is_empty(),
        "A stuck (head-on)"
    );
    assert!(
        world.get::<Movement>(b).unwrap().path.is_empty(),
        "B stuck (head-on)"
    );
    assert_eq!(*world.get::<TilePos>(a).unwrap(), TilePos::new(5, 2));
    assert_eq!(*world.get::<TilePos>(b).unwrap(), TilePos::new(1, 2));

    // Diagonal merge: C from (1,1) to (3,3), D from (3,1) to (1,3) — their
    // routes cross in the middle. Both must arrive.
    let c = world
        .spawn((
            TilePos::new(1, 1),
            crew("C", 3.0),
            CrewTask::default(),
            Movement {
                path: vec![TilePos::new(2, 2), TilePos::new(3, 3)],
                ..default()
            },
        ))
        .id();
    let d = world
        .spawn((
            TilePos::new(3, 1),
            crew("D", 3.0),
            CrewTask::default(),
            Movement {
                path: vec![TilePos::new(2, 2), TilePos::new(1, 3)],
                ..default()
            },
        ))
        .id();
    let mut t = 0.0;
    while t < 30.0 {
        world
            .resource_mut::<ship_alive::simtime::SimClock>()
            .advance_sim(1.0 / 60.0 * ship_alive::simtime::BASE_SIM_RATE);
        schedule.run(&mut world);
        t += 1.0 / 60.0;
        if world.get::<Movement>(c).unwrap().path.is_empty()
            && world.get::<Movement>(d).unwrap().path.is_empty()
        {
            break;
        }
    }
    assert!(
        world.get::<Movement>(c).unwrap().path.is_empty(),
        "C stuck (diagonal merge)"
    );
    assert!(
        world.get::<Movement>(d).unwrap().path.is_empty(),
        "D stuck (diagonal merge)"
    );
}

// K2 — one-wide door squeeze with 8-way traffic keeps draining.
#[test]
fn one_wide_door_drains() {
    let rows = &["#######", "##.####", "#.....#", "#######"];
    let mut world = movement_world(rows);
    let mut schedule = Schedule::default();
    schedule.add_systems((ship_alive::movement::movement_system,));

    // Three crew queue through the single door (2,1) to row 2.
    let mut crew_ids = Vec::new();
    for (i, start) in [
        (0, TilePos::new(1, 2)),
        (1, TilePos::new(2, 2)),
        (2, TilePos::new(3, 2)),
    ] {
        let _ = i;
        let e = world
            .spawn((
                start,
                crew("q", 3.0),
                CrewTask::default(),
                Movement {
                    path: vec![TilePos::new(4, 2), TilePos::new(5, 2)],
                    ..default()
                },
            ))
            .id();
        crew_ids.push(e);
    }
    let mut t = 0.0;
    while t < 30.0 {
        world
            .resource_mut::<ship_alive::simtime::SimClock>()
            .advance_sim(1.0 / 60.0 * ship_alive::simtime::BASE_SIM_RATE);
        schedule.run(&mut world);
        t += 1.0 / 60.0;
        if crew_ids
            .iter()
            .all(|e| world.get::<Movement>(*e).unwrap().path.is_empty())
        {
            break;
        }
    }
    for e in crew_ids {
        assert!(
            world.get::<Movement>(e).unwrap().path.is_empty(),
            "crew stuck at the door after {t:.1}s"
        );
    }
}
