//! Headless integration tests for Slice 1 core loops: construction,
//! deconstruction, production, storage filters and work priorities.
//! Runs the real jobs systems on a bare `bevy_ecs` World.

use bevy::ecs::schedule::Schedule;
use bevy::ecs::world::World;
use bevy::prelude::*;
use ship_alive::building::{Blueprint, Building, BuildingKind, Footprint, MarkedForDeconstruct};
use ship_alive::crew::{Crew, CrewTask, IdleCause, Movement, Priority, WorkKind};
use ship_alive::items::{Item, ItemKind, MarkedForHaul, ReservedBy};
use ship_alive::jobs::{self, Action};
use ship_alive::log::EventLog;
use ship_alive::map::{ShipMap, Tile, TilePos};
use ship_alive::production::{Fabricator, MachineState};
use ship_alive::storage::StorageCell;
use std::time::Duration;

/// Roomy test ship: open floor with a rack at (5,1) and a fabricator 2x2 at
/// (4,3)-(5,4) placed manually by the tests that need it.
const LAYOUT: [&str; 7] = [
    "#########",
    "#C....S.#",
    "#.......#",
    "#...#####",
    "#...#...#",
    "#...#...#",
    "#########",
];

fn spawn_crew(world: &mut World, name: &str, pos: TilePos) -> Entity {
    let mut crew = Crew::new(name, Color::WHITE);
    crew.next_scan = 0.0;
    world
        .spawn((pos, crew, CrewTask::default(), Movement::default()))
        .id()
}

fn spawn_item(world: &mut World, pos: TilePos, kind: ItemKind) -> Entity {
    world.spawn((pos, Item { kind })).id()
}

fn spawn_rack(world: &mut World, pos: TilePos) -> Entity {
    world
        .spawn((
            pos,
            StorageCell::default(),
            Footprint::new(pos.x, pos.y, 1, 1),
            Building {
                kind: BuildingKind::Rack,
                foot: Footprint::new(pos.x, pos.y, 1, 1),
                demo_progress: 0.0,
            },
        ))
        .id()
}

fn spawn_fab(world: &mut World, pos: TilePos) -> Entity {
    world.resource_scope(|world, mut map: Mut<ShipMap>| {
        for dy in 0..2 {
            for dx in 0..2 {
                map.set_tile(TilePos::new(pos.x + dx, pos.y + dy), Tile::Machine);
            }
        }
        world
            .spawn((
                pos,
                Footprint::new(pos.x, pos.y, 2, 2),
                Building {
                    kind: BuildingKind::Fabricator,
                    foot: Footprint::new(pos.x, pos.y, 2, 2),
                    demo_progress: 0.0,
                },
                Fabricator::default(),
                ship_alive::power::PowerRole::consumer(ship_alive::power::FABRICATOR_DEMAND),
                ship_alive::power::PowerStatus::default(),
            ))
            .id()
    });
    // Feed it power: a reactor three tiles west with a short cable run.
    world.resource_scope(
        |_w: &mut World, mut cables: Mut<ship_alive::power::CableGrid>| {
            for y in [pos.y, pos.y + 1] {
                for x in (pos.x - 2)..pos.x {
                    cables.set(TilePos::new(x, y), true);
                }
            }
        },
    );
    let _gen = world
        .spawn((
            TilePos::new(pos.x - 4, pos.y),
            Footprint::new(pos.x - 4, pos.y, 2, 2),
            ship_alive::power::PowerRole::generator(),
            ship_alive::power::PowerStatus::default(),
        ))
        .id();
    world.resource_scope(
        |world: &mut World, _p: Mut<ship_alive::power::PowerState>| {
            world
                .query_filtered::<Entity, With<Fabricator>>()
                .iter(world)
                .next()
                .unwrap()
        },
    )
}

struct Harness {
    world: World,
    schedule: Schedule,
}

impl Harness {
    fn new() -> Self {
        Self::with_layout(&LAYOUT)
    }

    fn with_layout(layout: &[&str]) -> Self {
        let mut world = World::new();
        let (map, _) = ShipMap::from_layout(layout);
        let (w, h) = (map.width, map.height);
        world.insert_resource(map);
        world.insert_resource(EventLog::default());
        world.insert_resource(ship_alive::stats::Stats::default());
        world.insert_resource(ship_alive::power::CableGrid::new(w, h));
        world.insert_resource(ship_alive::power::PowerState::default());
        world.insert_resource(Time::<Virtual>::default());
        world.init_resource::<Events<Action>>();
        let mut schedule = Schedule::default();
        schedule.add_systems(
            (
                jobs::actions_system,
                ship_alive::power::power_network_system,
                jobs::crew_task_system,
                jobs::crew_scan_system,
                ship_alive::movement::movement_system,
            )
                .chain(),
        );
        Self { world, schedule }
    }

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

    fn crew_task(&self, e: Entity) -> CrewTask {
        self.world.get::<CrewTask>(e).unwrap().clone()
    }

    fn task_kind(&self, e: Entity) -> &'static str {
        match self.crew_task(e) {
            CrewTask::Idle(_) => "idle",
            CrewTask::Haul(_) => "haul",
            CrewTask::Build(_) => "build",
            CrewTask::Deconstruct(_) => "deconstruct",
            CrewTask::Operate(_) => "operate",
        }
    }
}

fn set_priority(world: &mut World, crew: Entity, work: WorkKind, level: Priority) {
    world
        .get_mut::<Crew>(crew)
        .unwrap()
        .priorities
        .set(work, level);
}

// =====================================================================================
// Construction
// =====================================================================================

#[test]
fn blueprint_is_supplied_and_built_end_to_end() {
    let mut h = Harness::new();
    let crew = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let _rack = spawn_rack(&mut h.world, TilePos::new(5, 1));
    // One part on the ground next to where the rack will be built.
    spawn_item(&mut h.world, TilePos::new(4, 2), ItemKind::Part);

    h.send(Action::PlaceBlueprint {
        kind: BuildingKind::Rack,
        pos: TilePos::new(3, 2),
    });
    h.step(0.1);
    let bps: Vec<Entity> = h
        .world
        .query::<(Entity, &Blueprint)>()
        .iter(&h.world)
        .map(|(e, _)| e)
        .collect();
    assert_eq!(bps.len(), 1);

    // Supply + construction: 60 game-seconds is plenty on this tiny ship.
    h.steps(0.05, 1200);

    let buildings: Vec<(Entity, &Building)> = h
        .world
        .query::<(Entity, &Building)>()
        .iter(&h.world)
        .collect();
    assert!(
        buildings
            .iter()
            .any(|(_, b)| b.kind == BuildingKind::Rack && b.foot.x == 3 && b.foot.y == 2),
        "rack building should exist after construction, got {buildings:?}"
    );
    assert!(
        h.world
            .query::<(Entity, &Blueprint)>()
            .iter(&h.world)
            .next()
            .is_none(),
        "blueprint consumed"
    );
    assert!(matches!(h.crew_task(crew), CrewTask::Idle(_)));
    // The part item was delivered onto the blueprint (no longer a loose ground item there).
    let loose: Vec<Entity> = h
        .world
        .query_filtered::<(Entity, &Item), Without<MarkedForHaul>>()
        .iter(&h.world)
        .filter(|(_, it)| it.kind == ItemKind::Part)
        .map(|(e, _)| e)
        .collect();
    assert!(
        loose.is_empty(),
        "the part should have been consumed by the build"
    );
}

#[test]
fn invalid_placements_are_rejected() {
    let mut h = Harness::new();
    spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let _rack = spawn_rack(&mut h.world, TilePos::new(5, 1));

    // On a hull wall.
    h.send(Action::PlaceBlueprint {
        kind: BuildingKind::Wall,
        pos: TilePos::new(0, 0),
    });
    // Overlapping the existing rack.
    h.send(Action::PlaceBlueprint {
        kind: BuildingKind::Wall,
        pos: TilePos::new(5, 1),
    });
    // Item in the way of a wall.
    spawn_item(&mut h.world, TilePos::new(3, 2), ItemKind::Crate);
    h.send(Action::PlaceBlueprint {
        kind: BuildingKind::Wall,
        pos: TilePos::new(3, 2),
    });
    h.step(0.1);

    assert_eq!(
        h.world
            .query::<(Entity, &Blueprint)>()
            .iter(&h.world)
            .count(),
        0
    );
    let log = h.world.get_resource::<EventLog>().unwrap();
    assert!(log.entries.iter().any(|e| e.text.contains("Cannot build")));
}

#[test]
fn material_claim_is_exclusive_between_crew() {
    let mut h = Harness::new();
    let c1 = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let c2 = spawn_crew(&mut h.world, "B", TilePos::new(1, 5));
    let _rack = spawn_rack(&mut h.world, TilePos::new(5, 1));
    spawn_item(&mut h.world, TilePos::new(3, 3), ItemKind::Part); // the only part
    h.send(Action::PlaceBlueprint {
        kind: BuildingKind::Rack,
        pos: TilePos::new(2, 5),
    });
    h.step(0.1);

    let kinds = [h.task_kind(c1), h.task_kind(c2)];
    let haulers = kinds.iter().filter(|k| **k == "haul").count();
    assert_eq!(haulers, 1, "exactly one crew should claim the supply haul");
    let item_reserved = h
        .world
        .query_filtered::<&Item, With<ReservedBy>>()
        .iter(&h.world)
        .count();
    assert_eq!(item_reserved, 1);
}

#[test]
fn build_job_is_not_double_claimed() {
    let mut h = Harness::new();
    let c1 = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let c2 = spawn_crew(&mut h.world, "B", TilePos::new(1, 5));
    let _rack = spawn_rack(&mut h.world, TilePos::new(5, 1));
    spawn_item(&mut h.world, TilePos::new(3, 3), ItemKind::Part);
    h.send(Action::PlaceBlueprint {
        kind: BuildingKind::Rack,
        pos: TilePos::new(2, 5),
    });
    // Let supply finish, then both crews are free when the build starts.
    h.steps(0.05, 1200);

    // After everything settles the rack exists and both crews are idle again.
    assert_eq!(h.task_kind(c1), "idle");
    assert_eq!(h.task_kind(c2), "idle");
    let racks = h
        .world
        .query::<(Entity, &Building)>()
        .iter(&h.world)
        .filter(|(_, b)| b.kind == BuildingKind::Rack)
        .count();
    assert_eq!(racks, 2, "starter rack + built rack");
    // And the blueprint's reservation was cleaned up with the entity.
    assert!(h
        .world
        .query::<(Entity, &Blueprint)>()
        .iter(&h.world)
        .next()
        .is_none());
}

#[test]
fn cancel_blueprint_refunds_delivered_materials() {
    let mut h = Harness::new();
    let _crew = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let _rack = spawn_rack(&mut h.world, TilePos::new(5, 1));
    // No crew can run: make one crew with all work disabled so the material
    // never gets delivered — instead pre-deliver by hand.
    let bp = {
        let e = h
            .world
            .spawn((
                TilePos::new(3, 2),
                Footprint::new(3, 2, 1, 1),
                Blueprint {
                    kind: BuildingKind::Rack,
                    foot: Footprint::new(3, 2, 1, 1),
                    delivered: [0, 0, 1], // one part on site
                    progress: 0.0,
                },
            ))
            .id();
        e
    };
    h.send(Action::CancelBlueprint { blueprint: bp });
    h.step(0.1);

    assert!(h.world.get_entity(bp).is_err(), "blueprint despawned");
    let parts: Vec<Entity> = h
        .world
        .query::<(Entity, &Item)>()
        .iter(&h.world)
        .filter(|(_, it)| it.kind == ItemKind::Part)
        .map(|(e, _)| e)
        .collect();
    assert_eq!(parts.len(), 1, "delivered part refunded to the ground");
}

#[test]
fn wall_blocks_pathing_and_deconstruct_restores_it() {
    // Corridor ship: (5,2) is the only passage between left and right rooms.
    let layout = ["###########", "#....#....#", "#.........#", "###########"];
    let mut h = Harness::with_layout(&layout);
    let _crew = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let _rack = spawn_rack(&mut h.world, TilePos::new(6, 1));

    let path = {
        let map = h.world.get_resource::<ShipMap>().unwrap();
        ship_alive::path::find_path(map, TilePos::new(1, 1), TilePos::new(7, 1), |_| false)
    };
    assert!(path.is_some(), "passage exists before the wall is built");

    // Spawn a finished wall building on the passage (as construction would).
    let wall = h
        .world
        .spawn((
            TilePos::new(5, 2),
            Footprint::new(5, 2, 1, 1),
            Building {
                kind: BuildingKind::Wall,
                foot: Footprint::new(5, 2, 1, 1),
                demo_progress: 0.0,
            },
        ))
        .id();
    h.world.resource_scope(|_w, mut m: Mut<ShipMap>| {
        m.set_tile(TilePos::new(5, 2), Tile::BuiltWall);
    });

    let blocked = {
        let map = h.world.get_resource::<ShipMap>().unwrap();
        ship_alive::path::find_path(map, TilePos::new(1, 1), TilePos::new(7, 1), |_| false)
    };
    assert!(blocked.is_none(), "built wall must cut the corridor");

    // Deconstruct it through the work system.
    h.send(Action::MarkDeconstruct { building: wall });
    h.step(0.1);
    assert!(h.world.get::<MarkedForDeconstruct>(wall).is_some());
    h.steps(0.05, 400);

    assert!(h.world.get_entity(wall).is_err(), "wall removed");
    let restored = {
        let map = h.world.get_resource::<ShipMap>().unwrap();
        ship_alive::path::find_path(map, TilePos::new(1, 1), TilePos::new(7, 1), |_| false)
    };
    assert!(
        restored.is_some(),
        "deconstruction must restore the corridor"
    );
    // Full refund: one part on the ground.
    let parts = h
        .world
        .query::<(Entity, &Item)>()
        .iter(&h.world)
        .filter(|(_, it)| it.kind == ItemKind::Part)
        .count();
    assert_eq!(parts, 1);
}

// =====================================================================================
// Production
// =====================================================================================

#[test]
fn production_runs_end_to_end() {
    let mut h = Harness::new();
    let crew = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let _rack = spawn_rack(&mut h.world, TilePos::new(5, 1));
    let fab = spawn_fab(&mut h.world, TilePos::new(4, 3));
    // Ore far from the machine (still on this tiny ship).
    spawn_item(&mut h.world, TilePos::new(1, 5), ItemKind::Ore);
    spawn_item(&mut h.world, TilePos::new(2, 5), ItemKind::Ore);

    h.send(Action::FabAddOrder { fab, batches: 1 });
    h.step(0.1);

    // Ore hauled in, machine operated, part produced, part hauled to storage.
    h.steps(0.05, 3000);

    let (out_parts, order_gone) = {
        let f = h.world.get::<Fabricator>(fab).unwrap();
        (f.output[ItemKind::Part.index()], f.order.is_none())
    };
    assert_eq!(out_parts, 0, "output should have been hauled away");
    let rack_counts = h
        .world
        .query::<(Entity, &StorageCell)>()
        .iter(&h.world)
        .next()
        .map(|(_, c)| c.counts)
        .unwrap();
    assert!(
        rack_counts[ItemKind::Part.index()] >= 1,
        "part stored in rack: {rack_counts:?}"
    );
    assert!(order_gone, "single-batch order consumed");
    assert!(matches!(h.crew_task(crew), CrewTask::Idle(_)));
}

#[test]
fn no_order_or_no_input_means_no_operate() {
    let mut h = Harness::new();
    let crew = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let _rack = spawn_rack(&mut h.world, TilePos::new(5, 1));
    let fab = spawn_fab(&mut h.world, TilePos::new(4, 3));
    h.step(0.1);
    // No order: nobody operates.
    assert_eq!(h.task_kind(crew), "idle");

    h.send(Action::FabAddOrder { fab, batches: 1 });
    h.step(0.2);
    let f = h.world.get::<Fabricator>(fab).unwrap();
    assert_eq!(
        f.state(),
        MachineState::WaitingInput,
        "no ore anywhere → waiting for input"
    );
    assert_eq!(h.task_kind(crew), "idle");
}

#[test]
fn operate_reservation_is_exclusive() {
    let mut h = Harness::new();
    let c1 = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let c2 = spawn_crew(&mut h.world, "B", TilePos::new(1, 5));
    let _rack = spawn_rack(&mut h.world, TilePos::new(5, 1));
    let fab = spawn_fab(&mut h.world, TilePos::new(4, 3));
    // Pre-load input so the machine is immediately ready for a worker.
    {
        let mut f = h.world.get_mut::<Fabricator>(fab).unwrap();
        f.order = Some(ship_alive::production::Order {
            batches: 1,
            repeat: false,
        });
        f.input[ItemKind::Ore.index()] = 2;
    }
    h.step(0.1);

    let res = h.world.get::<ReservedBy>(fab).map(|r| r.0);
    assert!(
        res == Some(c1) || res == Some(c2),
        "machine reserved by exactly one crew"
    );
    let operators = [h.task_kind(c1), h.task_kind(c2)]
        .iter()
        .filter(|k| **k == "operate")
        .count();
    assert_eq!(operators, 1, "exactly one operator claims the machine");
}

#[test]
fn output_blocked_stops_production() {
    let mut h = Harness::new();
    let crew = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let rack = spawn_rack(&mut h.world, TilePos::new(5, 1));
    // The rack refuses parts, so the full output buffer cannot be emptied:
    // the blockage is real and persistent.
    {
        let mut c = h.world.get_mut::<StorageCell>(rack).unwrap();
        c.allowed = [true, true, false];
    }
    let fab = spawn_fab(&mut h.world, TilePos::new(4, 3));
    {
        let mut f = h.world.get_mut::<Fabricator>(fab).unwrap();
        f.order = Some(ship_alive::production::Order {
            batches: 3,
            repeat: false,
        });
        f.input[ItemKind::Ore.index()] = 2;
        f.output[ItemKind::Part.index()] = Fabricator::OUTPUT_CAP; // full
    }
    h.steps(0.05, 100);
    assert_eq!(
        h.task_kind(crew),
        "idle",
        "no operate job while output is blocked"
    );
    assert_eq!(
        h.world.get::<Fabricator>(fab).unwrap().state(),
        MachineState::OutputBlocked
    );
}

#[test]
fn clearing_order_mid_cycle_keeps_material() {
    let mut h = Harness::new();
    let crew = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let _rack = spawn_rack(&mut h.world, TilePos::new(5, 1));
    let fab = spawn_fab(&mut h.world, TilePos::new(4, 3));
    {
        let mut f = h.world.get_mut::<Fabricator>(fab).unwrap();
        f.order = Some(ship_alive::production::Order {
            batches: 1,
            repeat: false,
        });
        f.input[ItemKind::Ore.index()] = 2;
    }
    // Let the crew claim and start operating.
    h.steps(0.05, 40);
    assert_eq!(h.task_kind(crew), "operate");
    h.send(Action::FabClearOrder { fab });
    h.step(0.1);
    h.step(0.1);

    let f = h.world.get::<Fabricator>(fab).unwrap();
    assert!(!f.active);
    assert_eq!(
        f.input[ItemKind::Ore.index()],
        2,
        "aborted cycle must not consume ore"
    );
    assert_eq!(f.output[ItemKind::Part.index()], 0);
    assert!(matches!(h.crew_task(crew), CrewTask::Idle(_)));
    assert!(
        h.world.get::<ReservedBy>(fab).is_none(),
        "machine reservation released"
    );
}

// =====================================================================================
// Storage filters + auto-logistics
// =====================================================================================

#[test]
fn rack_filters_route_kinds_to_the_right_racks() {
    let mut h = Harness::new();
    let _crew = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let ore_rack = spawn_rack(&mut h.world, TilePos::new(5, 1));
    let part_rack = spawn_rack(&mut h.world, TilePos::new(5, 2));
    {
        let mut c = h.world.get_mut::<StorageCell>(ore_rack).unwrap();
        c.allowed = [false, true, false]; // ore only
    }
    {
        let mut c = h.world.get_mut::<StorageCell>(part_rack).unwrap();
        c.allowed = [false, false, true]; // part only
    }
    let ore = spawn_item(&mut h.world, TilePos::new(1, 2), ItemKind::Ore);
    let part = spawn_item(&mut h.world, TilePos::new(1, 3), ItemKind::Part);
    h.world.entity_mut(ore).insert(MarkedForHaul);
    h.world.entity_mut(part).insert(MarkedForHaul);

    h.steps(0.05, 1500);

    let ore_cell = h.world.get::<StorageCell>(ore_rack).unwrap();
    let part_cell = h.world.get::<StorageCell>(part_rack).unwrap();
    assert_eq!(ore_cell.counts[ItemKind::Ore.index()], 1);
    assert_eq!(ore_cell.counts[ItemKind::Part.index()], 0);
    assert_eq!(part_cell.counts[ItemKind::Part.index()], 1);
    assert_eq!(part_cell.counts[ItemKind::Ore.index()], 0);
}

#[test]
fn auto_supply_pulls_from_rack_stock() {
    let mut h = Harness::new();
    let _crew = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let rack = spawn_rack(&mut h.world, TilePos::new(5, 1));
    {
        let mut c = h.world.get_mut::<StorageCell>(rack).unwrap();
        c.counts[ItemKind::Part.index()] = 2;
    }
    h.send(Action::PlaceBlueprint {
        kind: BuildingKind::Wall,
        pos: TilePos::new(2, 3),
    });
    h.step(0.1);

    // The wall blueprint needs 1 part; the only source is rack stock.
    h.steps(0.05, 1200);
    let walls = h
        .world
        .query::<(Entity, &Building)>()
        .iter(&h.world)
        .filter(|(_, b)| b.kind == BuildingKind::Wall)
        .count();
    assert_eq!(walls, 1, "wall built from rack stock");
    let c = h.world.get::<StorageCell>(rack).unwrap();
    assert_eq!(
        c.counts[ItemKind::Part.index()],
        1,
        "one part pulled out of the rack"
    );
}

// =====================================================================================
// Work priorities
// =====================================================================================

#[test]
fn disabled_work_type_is_never_claimed() {
    let mut h = Harness::new();
    let crew = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let _rack = spawn_rack(&mut h.world, TilePos::new(5, 1));
    let _item = spawn_item(&mut h.world, TilePos::new(3, 2), ItemKind::Crate);
    set_priority(&mut h.world, crew, WorkKind::Haul, Priority::Disabled);
    h.steps(0.05, 100);
    assert_eq!(h.task_kind(crew), "idle");
    let CrewTask::Idle(cause) = h.crew_task(crew) else {
        panic!()
    };
    assert_eq!(cause, IdleCause::NothingToDo);
}

#[test]
fn high_priority_tier_beats_low() {
    let mut h = Harness::new();
    let crew = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let _rack = spawn_rack(&mut h.world, TilePos::new(5, 1));
    // A marked item right next to the crew (haul work, distance 1).
    let near = spawn_item(&mut h.world, TilePos::new(2, 1), ItemKind::Crate);
    h.world.entity_mut(near).insert(MarkedForHaul);
    // A fully supplied wall blueprint far away (build work, distance ~5).
    let bp = h
        .world
        .spawn((
            TilePos::new(3, 5),
            Footprint::new(3, 5, 1, 1),
            Blueprint {
                kind: BuildingKind::Wall,
                foot: Footprint::new(3, 5, 1, 1),
                delivered: [0, 0, 1], // supplied
                progress: 0.0,
            },
        ))
        .id();
    set_priority(&mut h.world, crew, WorkKind::Haul, Priority::Low);
    set_priority(&mut h.world, crew, WorkKind::Build, Priority::High);
    h.step(0.1);
    assert_eq!(
        h.task_kind(crew),
        "build",
        "High build must beat the nearer Low haul"
    );
    let _ = bp;
}

#[test]
fn mixed_work_distributes_without_conflicts() {
    let mut h = Harness::new();
    let c1 = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let c2 = spawn_crew(&mut h.world, "B", TilePos::new(1, 5));
    let _rack = spawn_rack(&mut h.world, TilePos::new(5, 1));
    let fab = spawn_fab(&mut h.world, TilePos::new(4, 3));
    {
        let mut f = h.world.get_mut::<Fabricator>(fab).unwrap();
        f.order = Some(ship_alive::production::Order {
            batches: 2,
            repeat: false,
        });
        f.input[ItemKind::Ore.index()] = 2;
    }
    let item = spawn_item(&mut h.world, TilePos::new(2, 2), ItemKind::Crate);
    h.world.entity_mut(item).insert(MarkedForHaul);
    set_priority(&mut h.world, c1, WorkKind::Operate, Priority::High);
    set_priority(&mut h.world, c2, WorkKind::Haul, Priority::High);
    h.step(0.1);
    assert_eq!(h.task_kind(c1), "operate");
    assert_eq!(h.task_kind(c2), "haul");

    // Stability: run a while; work may still be draining but no duplicate
    // reservations may exist and neither crew may hold a phantom job.
    h.steps(0.05, 2500);
    let reserved_count = h
        .world
        .query_filtered::<Entity, With<ReservedBy>>()
        .iter(&h.world)
        .count();
    let active_jobs = [h.task_kind(c1), h.task_kind(c2)]
        .iter()
        .filter(|k| **k != "idle")
        .count();
    assert_eq!(
        reserved_count, active_jobs,
        "reservations match live jobs exactly"
    );
}

// =====================================================================================
// Deconstruct rack drops contents
// =====================================================================================

#[test]
fn deconstructing_a_rack_refunds_cost_and_contents() {
    let mut h = Harness::new();
    let _crew = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let rack = spawn_rack(&mut h.world, TilePos::new(5, 1));
    {
        let mut c = h.world.get_mut::<StorageCell>(rack).unwrap();
        c.counts[ItemKind::Ore.index()] = 2;
    }
    h.send(Action::MarkDeconstruct { building: rack });
    h.steps(0.05, 400);

    assert!(h.world.get_entity(rack).is_err());
    let mut parts = 0;
    let mut ores = 0;
    for (_, it) in h.world.query::<(Entity, &Item)>().iter(&h.world) {
        match it.kind {
            ItemKind::Part => parts += 1,
            ItemKind::Ore => ores += 1,
            _ => {}
        }
    }
    assert_eq!(parts, 1, "build cost refunded (rack = 1 part)");
    assert_eq!(ores, 2, "stored contents dropped");
}
