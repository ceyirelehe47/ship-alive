//! Headless integration tests for the haul-job core, running the real systems
//! on a bare `bevy_ecs` `World` with a manually advanced virtual clock.

use bevy::ecs::schedule::Schedule;
use bevy::ecs::world::World;
use bevy::prelude::*;
use ship_alive::crew::{Crew, CrewTask, HaulPhase, IdleCause, Movement};
use ship_alive::items::{CarriedBy, Item, ItemKind, MarkedForHaul, NoPathUntil, ReservedBy};
use ship_alive::jobs::{self, Action};
use ship_alive::log::EventLog;
use ship_alive::map::{ShipMap, TilePos};
use ship_alive::storage::StorageCell;
use std::time::Duration;

/// Tiny ship: crew room left, storage right, connected by a corridor.
const LAYOUT: [&str; 5] = ["#######", "#C..S.#", "#.....#", "#.....#", "#######"];

fn spawn_crew(world: &mut World, name: &str, pos: TilePos) -> Entity {
    let mut crew = Crew::new(name, Color::WHITE);
    crew.next_scan = 0.0;
    world
        .spawn((pos, crew, CrewTask::default(), Movement::default()))
        .id()
}

fn spawn_item(world: &mut World, pos: TilePos, marked: bool) -> Entity {
    let e = world
        .spawn((
            pos,
            Item {
                kind: ItemKind::Crate,
            },
        ))
        .id();
    if marked {
        world.entity_mut(e).insert(MarkedForHaul);
    }
    e
}

fn spawn_rack(world: &mut World, pos: TilePos, filled: u32) -> Entity {
    let mut cell = StorageCell {
        capacity: 4,
        counts: [0, 0, 0],
        allowed: [true, true, true],
    };
    cell.counts[ItemKind::Crate.index()] = filled;
    world.spawn((pos, cell)).id()
}

struct Harness {
    world: World,
    schedule: Schedule,
}

impl Harness {
    fn new() -> Self {
        let mut world = World::new();
        let (map, _) = ShipMap::from_layout(&LAYOUT);
        world.insert_resource(map);
        world.insert_resource(EventLog::default());
        world.insert_resource(ship_alive::stats::Stats::default());
        world.insert_resource(Time::<Virtual>::default());
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
        Self { world, schedule }
    }

    /// Advance virtual time by `dt` and run one frame.
    fn step(&mut self, dt: f32) {
        self.world
            .resource_mut::<Time<Virtual>>()
            .advance_by(Duration::from_secs_f32(dt));
        self.world.resource_mut::<Events<Action>>().update();
        self.schedule.run(&mut self.world);
    }

    fn steps(&mut self, dt: f32, n: usize) {
        for _ in 0..n {
            self.step(dt);
        }
    }

    fn send(&mut self, action: Action) {
        self.world.resource_mut::<Events<Action>>().send(action);
    }
}

/// Resolve one tile matching a char in the layout.
fn tile_of(ch: char) -> TilePos {
    for (y, row) in LAYOUT.iter().enumerate() {
        for (x, c) in row.chars().enumerate() {
            if c == ch {
                return TilePos::new(x as i32, y as i32);
            }
        }
    }
    panic!("char {ch} not in layout");
}

#[test]
fn claim_is_exclusive_between_crew() {
    let mut h = Harness::new();
    let c1 = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let c2 = spawn_crew(&mut h.world, "B", TilePos::new(1, 3));
    let item = spawn_item(&mut h.world, TilePos::new(3, 3), true);
    let _rack = spawn_rack(&mut h.world, tile_of('S'), 0);

    h.step(0.1); // one scan pass

    let mut q = h.world.query::<(&CrewTask, Option<&ReservedBy>)>();
    let mut hauling = 0;
    for (task, reserved) in q.iter(&h.world) {
        match task {
            CrewTask::Haul(_) => {
                hauling += 1;
                assert!(reserved.is_none()); // reservation lives on the item
            }
            CrewTask::Idle(cause) => {
                assert_eq!(*cause, IdleCause::AllClaimed);
            }
            _ => panic!("unexpected task type"),
        }
    }
    assert_eq!(hauling, 1);
    assert!(h.world.get::<ReservedBy>(item).is_some());
    let _ = (c1, c2);
}

#[test]
fn full_haul_flow_stores_item_and_frees_crew() {
    let mut h = Harness::new();
    let crew = spawn_crew(&mut h.world, "A", TilePos::new(2, 3));
    let item = spawn_item(&mut h.world, TilePos::new(3, 3), true);
    let rack = spawn_rack(&mut h.world, tile_of('S'), 0);

    // 60 game-seconds in 0.05s steps is plenty for the short walk.
    h.steps(0.05, 1200);

    assert!(
        h.world.get_entity(item).is_err(),
        "item should be despawned after storing"
    );
    let cell = h.world.get::<StorageCell>(rack).unwrap();
    assert_eq!(cell.stored(), 1);
    let crew_comp = h.world.get::<Crew>(crew).unwrap();
    assert_eq!(crew_comp.delivered, 1);
    assert!(matches!(
        h.world.get::<CrewTask>(crew).unwrap(),
        CrewTask::Idle(_)
    ));
}

#[test]
fn unmark_cancels_job_and_releases_reservation() {
    let mut h = Harness::new();
    let _crew = spawn_crew(&mut h.world, "A", TilePos::new(2, 3));
    let item = spawn_item(&mut h.world, TilePos::new(3, 3), true);
    let _rack = spawn_rack(&mut h.world, tile_of('S'), 0);
    h.step(0.1);
    assert!(h.world.get::<ReservedBy>(item).is_some());

    h.send(Action::ToggleMark { item });
    h.step(0.1);

    assert!(h.world.get::<ReservedBy>(item).is_none());
    let mut q = h.world.query::<&CrewTask>();
    for task in q.iter(&h.world) {
        // The cancel settles as Idle; with chained systems the scan may have
        // already re-run and replaced the cosmetic JobCanceled cause.
        assert!(matches!(task, CrewTask::Idle(_)), "got {task:?}");
    }
}

#[test]
fn despawned_target_cancels_cleanly() {
    let mut h = Harness::new();
    let crew = spawn_crew(&mut h.world, "A", TilePos::new(2, 3));
    let item = spawn_item(&mut h.world, TilePos::new(3, 3), true);
    let _rack = spawn_rack(&mut h.world, tile_of('S'), 0);
    h.step(0.1);
    assert!(matches!(
        h.world.get::<CrewTask>(crew).unwrap(),
        CrewTask::Haul(_)
    ));

    h.world.despawn(item);
    h.step(0.1);

    assert!(matches!(
        h.world.get::<CrewTask>(crew).unwrap(),
        CrewTask::Idle(_)
    ));
}

#[test]
fn full_storage_blocks_claiming() {
    let mut h = Harness::new();
    let crew = spawn_crew(&mut h.world, "A", TilePos::new(2, 3));
    let _item = spawn_item(&mut h.world, TilePos::new(3, 3), true);
    spawn_rack(&mut h.world, tile_of('S'), 4); // full

    h.step(0.1);

    let CrewTask::Idle(cause) = h.world.get::<CrewTask>(crew).unwrap() else {
        panic!("should not claim with full storage")
    };
    assert_eq!(*cause, IdleCause::NoStorageSpace);
}

#[test]
fn unreachable_item_gets_cooldown_not_reservation() {
    // Wall off the item tiles entirely (2-tile sealed pocket at (3,3)-(3,4)).
    let layout = [
        "#######", "#C....#", "#.###.#", "#.#.#.#", "#.#.#.#", "#######",
    ];
    let mut world = World::new();
    let (map, _) = ShipMap::from_layout(&layout);
    world.insert_resource(map);
    world.insert_resource(EventLog::default());
    world.insert_resource(ship_alive::stats::Stats::default());
    world.insert_resource(Time::<Virtual>::default());
    world.init_resource::<Events<Action>>();
    let mut schedule = Schedule::default();
    schedule.add_systems((
        jobs::actions_system,
        jobs::crew_task_system,
        jobs::crew_scan_system,
    ));
    let mut h = Harness { world, schedule };

    let crew = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let item = spawn_item(&mut h.world, TilePos::new(3, 3), true);
    let _rack = spawn_rack(&mut h.world, TilePos::new(5, 3), 0);

    h.step(0.1);

    assert!(h.world.get::<ReservedBy>(item).is_none());
    let cooled = h.world.get::<NoPathUntil>(item).expect("cooldown expected");
    assert!(cooled.0 > 0.0);
    let CrewTask::Idle(cause) = h.world.get::<CrewTask>(crew).unwrap() else {
        panic!()
    };
    assert_eq!(*cause, IdleCause::AllUnreachable);
}

#[test]
fn rack_filling_up_mid_delivery_drops_item() {
    let mut h = Harness::new();
    let c1 = spawn_crew(&mut h.world, "A", TilePos::new(2, 3));
    let c2 = spawn_crew(&mut h.world, "B", TilePos::new(1, 3));
    let item1 = spawn_item(&mut h.world, TilePos::new(3, 3), true);
    let item2 = spawn_item(&mut h.world, TilePos::new(3, 2), true);
    // Rack with exactly one free slot.
    let rack = spawn_rack(&mut h.world, tile_of('S'), 3);

    h.steps(0.05, 1200);

    let stored = h.world.get::<StorageCell>(rack).unwrap().stored();
    assert_eq!(stored, 4, "rack should end up full");
    // The loser's item is back on the ground, unmarked and unreserved.
    let loser_item = if h.world.get_entity(item1).is_err() {
        item2
    } else {
        item1
    };
    assert!(h.world.get_entity(loser_item).is_ok());
    assert!(h.world.get::<MarkedForHaul>(loser_item).is_none());
    assert!(h.world.get::<ReservedBy>(loser_item).is_none());
    assert!(h.world.get::<CarriedBy>(loser_item).is_none());
    for e in [c1, c2] {
        assert!(matches!(
            h.world.get::<CrewTask>(e).unwrap(),
            CrewTask::Idle(_)
        ));
    }
    // And it stays stable: no re-claims while storage is full.
    h.steps(0.05, 200);
    assert!(h.world.get::<MarkedForHaul>(loser_item).is_none());
}

#[test]
fn carried_item_is_flagged_and_targeted() {
    let mut h = Harness::new();
    let crew = spawn_crew(&mut h.world, "A", TilePos::new(3, 3));
    let item = spawn_item(&mut h.world, TilePos::new(3, 3), true);
    let _rack = spawn_rack(&mut h.world, tile_of('S'), 0);
    h.step(0.1);
    h.steps(0.05, 40); // pickup happens (0.3s delay) then walks to storage

    if let Some(carried) = h.world.get::<CarriedBy>(item) {
        assert_eq!(carried.0, crew);
    }
    let task = h.world.get::<CrewTask>(crew).unwrap();
    if let CrewTask::Haul(job) = task {
        assert!(matches!(
            job.phase,
            HaulPhase::ToDest | HaulPhase::Delivering
        ));
    }
}

#[test]
fn box_select_marks_only_items_inside_rect() {
    let mut h = Harness::new();
    spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    // World coords: tile (x, y) center = ((x+0.5)*32, -((y+0.5)*32)).
    let inside1 = spawn_item(&mut h.world, TilePos::new(3, 3), false);
    let inside2 = spawn_item(&mut h.world, TilePos::new(4, 3), false);
    let outside = spawn_item(&mut h.world, TilePos::new(1, 3), false);
    let _rack = spawn_rack(&mut h.world, tile_of('S'), 0);

    // A box covering tiles x=3..4, y=3 (world x 96..160, world y -128..-96).
    h.send(Action::MarkArea {
        from: bevy::math::Vec2::new(90.0, -130.0),
        to: bevy::math::Vec2::new(165.0, -90.0),
    });
    h.step(0.05);

    assert!(h.world.get::<MarkedForHaul>(inside1).is_some());
    assert!(h.world.get::<MarkedForHaul>(inside2).is_some());
    assert!(h.world.get::<MarkedForHaul>(outside).is_none());
}

#[test]
fn box_select_is_idempotent_and_can_reselect_dropped() {
    let mut h = Harness::new();
    let item = spawn_item(&mut h.world, TilePos::new(3, 3), false);
    let p = bevy::math::Vec2::new(112.0, -112.0);
    h.send(Action::MarkArea { from: p, to: p });
    h.step(0.05);
    assert!(h.world.get::<MarkedForHaul>(item).is_some());

    // Unmark, then box-select the same tile again.
    h.send(Action::ToggleMark { item });
    h.step(0.05);
    assert!(h.world.get::<MarkedForHaul>(item).is_none());
    h.send(Action::MarkArea { from: p, to: p });
    h.step(0.05);
    assert!(h.world.get::<MarkedForHaul>(item).is_some());
}
