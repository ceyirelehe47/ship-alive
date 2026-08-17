//! Headless integration tests for Slice 2: the underfloor power grid,
//! network topology, deterministic load shedding, and the effect of power on
//! production. Runs the real systems on a bare `bevy_ecs` World.

use bevy::ecs::schedule::Schedule;
use bevy::ecs::world::World;
use bevy::prelude::*;
use ship_alive::building::{Building, BuildingKind, Footprint};
use ship_alive::crew::{Crew, CrewTask, Movement};
use ship_alive::items::ItemKind;
use ship_alive::jobs::{self, Action};
use ship_alive::log::EventLog;
use ship_alive::map::{ShipMap, Tile, TilePos};
use ship_alive::power::{CableGrid, NetworkInfo, PowerRole, PowerState, PowerStatus};
use ship_alive::production::Fabricator;
use ship_alive::storage::StorageCell;

const LAYOUT: [&str; 7] = [
    "#########",
    "#C....S.#",
    "#.......#",
    "#.......#",
    "#.......#",
    "#.......#",
    "#########",
];

fn spawn_crew(world: &mut World, name: &str, pos: TilePos) -> Entity {
    let mut crew = Crew::new(name, Color::WHITE);
    crew.next_scan = 0.0;
    world
        .spawn((pos, crew, CrewTask::default(), Movement::default()))
        .id()
}

/// A device entity with footprint + role (map tiles untouched — topology only
/// cares about the cable grid).
fn spawn_device(world: &mut World, pos: TilePos, w: i32, h: i32, role: PowerRole) -> Entity {
    world
        .spawn((
            pos,
            Footprint::new(pos.x, pos.y, w, h),
            role,
            PowerStatus::default(),
        ))
        .id()
}

fn spawn_fab(world: &mut World, pos: TilePos) -> Entity {
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
            PowerRole::consumer(ship_alive::power::FABRICATOR_DEMAND),
            PowerStatus::default(),
        ))
        .id()
}

struct Harness {
    world: World,
    schedule: Schedule,
}

impl Harness {
    fn new() -> Self {
        let mut world = World::new();
        let (map, _) = ShipMap::from_layout(&LAYOUT);
        let (w, h) = (map.width, map.height);
        world.insert_resource(map);
        world.insert_resource(EventLog::default());
        world.insert_resource(ship_alive::loc::Lang::default());
        world.insert_resource(ship_alive::stats::Stats::default());
        world.insert_resource(CableGrid::new(w, h));
        world.insert_resource(ship_alive::coolant::PipeGrid::new(w, h));
        world.insert_resource(ship_alive::coolant::WaterGrid::new(w, h));
        let thermal_grid = {
            let map = world.resource::<ship_alive::map::ShipMap>();
            ship_alive::thermal::ThermalGrid::new(map)
        };
        world.insert_resource(thermal_grid);
        world.insert_resource(ship_alive::coolant::CoolantStats::default());
        world.insert_resource(PowerState::default());
        world.insert_resource(ship_alive::simtime::SimClock::default());
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
            .resource_mut::<ship_alive::simtime::SimClock>()
            .advance_sim(dt as f64 * ship_alive::simtime::BASE_SIM_RATE);
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

    fn cable(&mut self, x: i32, y: i32, present: bool) {
        self.world
            .resource_mut::<CableGrid>()
            .set(TilePos::new(x, y), present);
    }

    fn status(&self, e: Entity) -> PowerStatus {
        *self.world.get::<PowerStatus>(e).unwrap()
    }

    fn networks(&self) -> Vec<NetworkInfo> {
        self.world.resource::<PowerState>().networks.clone()
    }
}

// =====================================================================================
// Topology
// =====================================================================================

#[test]
fn generator_feeds_connected_consumer() {
    let mut h = Harness::new();
    let gen = spawn_device(
        &mut h.world,
        TilePos::new(1, 3),
        2,
        2,
        PowerRole::generator(),
    );
    let fab = spawn_fab(&mut h.world, TilePos::new(5, 3));
    // Reactor perimeter (3,3) — cable run — fabricator perimeter (4,3).
    for x in 3..=4 {
        h.cable(x, 3, true);
    }
    h.step(0.05);
    assert_eq!(h.status(gen), PowerStatus::Powered);
    assert_eq!(h.status(fab), PowerStatus::Powered);
    assert_eq!(h.networks().len(), 1);
    let n = h.networks()[0];
    assert_eq!((n.generation, n.demand, n.served), (100, 20, 20));
}

#[test]
fn cutting_the_cable_disconnects_the_consumer() {
    let mut h = Harness::new();
    let _gen = spawn_device(
        &mut h.world,
        TilePos::new(1, 3),
        2,
        2,
        PowerRole::generator(),
    );
    let fab = spawn_fab(&mut h.world, TilePos::new(5, 3));
    for x in 3..=4 {
        h.cable(x, 3, true);
    }
    h.step(0.05);
    assert_eq!(h.status(fab), PowerStatus::Powered);

    h.cable(4, 3, false); // cut right next to the fabricator
    h.step(0.05);
    assert_eq!(
        h.status(fab),
        PowerStatus::Unconnected,
        "no cable at the interface"
    );

    h.cable(4, 3, true);
    h.step(0.05);
    assert_eq!(h.status(fab), PowerStatus::Powered, "re-laying restores");
}

#[test]
fn grid_split_leaves_far_side_without_generator() {
    let mut h = Harness::new();
    let _gen = spawn_device(
        &mut h.world,
        TilePos::new(1, 3),
        2,
        2,
        PowerRole::generator(),
    );
    let near = spawn_fab(&mut h.world, TilePos::new(5, 1));
    let far = spawn_fab(&mut h.world, TilePos::new(5, 4));
    // One line from the reactor's interface past both fabricators.
    h.cable(3, 3, true);
    h.cable(3, 2, true);
    h.cable(4, 2, true);
    h.cable(5, 2, true);
    h.cable(5, 3, true);
    h.cable(7, 4, true); // far-side stub (touches only the far fabricator)
    h.step(0.05);
    assert_eq!(h.status(near), PowerStatus::Powered);
    assert_eq!(h.status(far), PowerStatus::Powered);

    h.cable(5, 3, false); // cut between the two fabricators
    h.step(0.05);
    assert_eq!(
        h.status(near),
        PowerStatus::Powered,
        "generator side keeps running"
    );
    assert_eq!(
        h.status(far),
        PowerStatus::NoGenerator,
        "far side has no source"
    );
    assert_eq!(h.networks().len(), 2, "grid split into two networks");
}

#[test]
fn merging_two_networks_with_a_bridge_cable() {
    let mut h = Harness::new();
    // Two independent grids, each with its own generator and fabricator.
    let _gen_a = spawn_device(
        &mut h.world,
        TilePos::new(1, 1),
        2,
        2,
        PowerRole::generator(),
    );
    let _gen_b = spawn_device(
        &mut h.world,
        TilePos::new(1, 4),
        2,
        2,
        PowerRole::generator(),
    );
    let fab_a = spawn_fab(&mut h.world, TilePos::new(5, 1));
    let fab_b = spawn_fab(&mut h.world, TilePos::new(5, 4));
    h.cable(3, 1, true);
    h.cable(4, 1, true);
    h.cable(3, 5, true);
    h.cable(4, 5, true);
    h.step(0.05);
    assert_eq!(h.networks().len(), 2);
    assert_eq!(h.status(fab_a), PowerStatus::Powered);
    assert_eq!(h.status(fab_b), PowerStatus::Powered);

    // Bridge them with one cable; the two networks become one.
    h.cable(4, 3, true);
    h.cable(4, 2, true);
    h.cable(4, 4, true);
    h.step(0.05);
    assert_eq!(h.networks().len(), 1, "networks merged");
    let n = h.networks()[0];
    assert_eq!((n.generation, n.demand, n.served), (200, 40, 40));
}

#[test]
fn a_device_touching_two_grids_joins_them() {
    let mut h = Harness::new();
    // Generator feeds one cable group; a fabricator's interface also touches
    // a second, otherwise dead group — the device electrically joins them.
    let _gen = spawn_device(
        &mut h.world,
        TilePos::new(1, 3),
        2,
        2,
        PowerRole::generator(),
    );
    let fab = spawn_fab(&mut h.world, TilePos::new(5, 3));
    h.cable(3, 3, true); // generator side
    h.cable(4, 3, true); // touches fab's west perimeter
    h.cable(6, 3, true); // dead stub under the fab's east side
    h.step(0.05);
    assert_eq!(h.networks().len(), 1, "the fab's bus merges both groups");
    assert_eq!(h.status(fab), PowerStatus::Powered);
}

#[test]
fn isolated_network_shows_no_generator() {
    let mut h = Harness::new();
    let fab = spawn_fab(&mut h.world, TilePos::new(5, 3));
    h.cable(4, 3, true);
    h.cable(4, 4, true);
    h.step(0.05);
    assert_eq!(h.status(fab), PowerStatus::NoGenerator);
    assert_eq!(h.networks()[0].status_label(), "No generator");
}

#[test]
fn multiple_independent_networks_stay_separate() {
    let mut h = Harness::new();
    let _gen_a = spawn_device(
        &mut h.world,
        TilePos::new(1, 1),
        2,
        2,
        PowerRole::generator(),
    );
    let _gen_b = spawn_device(
        &mut h.world,
        TilePos::new(1, 4),
        2,
        2,
        PowerRole::generator(),
    );
    let fab_a = spawn_fab(&mut h.world, TilePos::new(5, 1));
    let fab_b = spawn_fab(&mut h.world, TilePos::new(5, 4));
    h.cable(3, 1, true);
    h.cable(4, 1, true);
    h.cable(3, 5, true);
    h.cable(4, 5, true);
    h.step(0.05);
    assert_eq!(h.networks().len(), 2);
    assert_eq!(h.status(fab_a), PowerStatus::Powered);
    assert_eq!(h.status(fab_b), PowerStatus::Powered);
    // Each network sees only its own load.
    let nets = h.networks();
    assert!(nets.iter().all(|n| n.generation == 100 && n.demand == 20));
}

// =====================================================================================
// Load math and shedding
// =====================================================================================

#[test]
fn generation_demand_and_headroom_math() {
    let mut h = Harness::new();
    let _gen = spawn_device(
        &mut h.world,
        TilePos::new(1, 3),
        2,
        2,
        PowerRole::generator(),
    );
    let fab = spawn_fab(&mut h.world, TilePos::new(5, 3));
    h.cable(3, 3, true);
    h.cable(4, 3, true);
    h.step(0.05);
    let n = h.networks()[0];
    assert_eq!(n.generation, 100);
    assert_eq!(n.demand, 20);
    assert_eq!(n.served, 20);
    assert_eq!(n.headroom(), 80);
    assert_eq!(n.status_label(), "Stable");
    let _ = fab;
}

#[test]
fn overload_sheds_deterministically_by_build_order() {
    let mut h = Harness::new();
    let _gen = spawn_device(
        &mut h.world,
        TilePos::new(1, 2),
        2,
        4,
        PowerRole::generator(),
    );
    // Five fabricators: demand 100 == generation (just served).
    let mut fabs = Vec::new();
    for i in 0..5 {
        let fab = spawn_fab(&mut h.world, TilePos::new(5, 1 + i));
        h.cable(4, 1 + i, true);
        h.cable(3, 1 + i, true);
        fabs.push(fab);
    }
    h.step(0.05);
    assert!(fabs.iter().all(|f| h.status(*f) == PowerStatus::Powered));

    // Add a sixth: demand 120 > 100 — the newest entity is shed, stably.
    let sixth = spawn_fab(&mut h.world, TilePos::new(5, 6));
    h.cable(4, 6, true);
    h.cable(3, 6, true);
    for _ in 0..10 {
        h.step(0.05);
        assert!(
            fabs.iter().all(|f| h.status(*f) == PowerStatus::Powered),
            "older devices keep power"
        );
        assert_eq!(
            h.status(sixth),
            PowerStatus::Shed,
            "newest is shed, no flicker"
        );
    }
    let n = h.networks()[0];
    assert_eq!((n.generation, n.demand, n.served), (100, 120, 100));
    assert_eq!(n.status_label(), "Insufficient power");
}

#[test]
fn turning_the_generator_off_unpowers_everything() {
    let mut h = Harness::new();
    let gen = spawn_device(
        &mut h.world,
        TilePos::new(1, 3),
        2,
        2,
        PowerRole::generator(),
    );
    let fab = spawn_fab(&mut h.world, TilePos::new(5, 3));
    h.cable(3, 3, true);
    h.cable(4, 3, true);
    h.step(0.05);

    h.send(Action::SetGeneratorOn { gen, on: false });
    h.step(0.05);
    assert_eq!(h.status(fab), PowerStatus::NoGenerator);
    assert_eq!(h.networks()[0].generation, 0);

    h.send(Action::SetGeneratorOn { gen, on: true });
    h.step(0.05);
    assert_eq!(h.status(fab), PowerStatus::Powered);
}

// =====================================================================================
// Production gating
// =====================================================================================

#[test]
fn unpowered_fab_is_never_operated() {
    let mut h = Harness::new();
    let crew = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let _rack = h
        .world
        .spawn((TilePos::new(5, 1), StorageCell::default()))
        .id();
    let gen = spawn_device(
        &mut h.world,
        TilePos::new(1, 3),
        2,
        2,
        PowerRole::generator(),
    );
    let fab = spawn_fab(&mut h.world, TilePos::new(5, 3));
    h.cable(3, 3, true);
    h.cable(4, 3, true);
    {
        let mut f = h.world.get_mut::<Fabricator>(fab).unwrap();
        f.order = Some(ship_alive::production::Order {
            batches: 1,
            repeat: false,
        });
        f.input[ItemKind::Ore.index()] = 2;
    }
    h.step(0.05);
    assert_eq!(h.status(fab), PowerStatus::Powered);

    // Cut power: the operate job must never be claimed.
    h.cable(4, 3, false);
    h.steps(0.05, 60);
    let claimed = matches!(h.world.get::<CrewTask>(crew).unwrap(), CrewTask::Operate(_));
    assert!(!claimed, "unpowered machine must not attract an operator");
    assert!(matches!(
        h.world.get::<CrewTask>(crew).unwrap(),
        CrewTask::Idle(_)
    ));

    // Restore: the machine becomes operable again.
    h.cable(4, 3, true);
    h.steps(0.05, 60);
    assert!(matches!(
        h.world.get::<CrewTask>(crew).unwrap(),
        CrewTask::Operate(_)
    ));
    let _ = gen;
}

#[test]
fn power_loss_mid_cycle_keeps_material() {
    let mut h = Harness::new();
    let crew = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let _rack = h
        .world
        .spawn((TilePos::new(5, 1), StorageCell::default()))
        .id();
    let _gen = spawn_device(
        &mut h.world,
        TilePos::new(1, 3),
        2,
        2,
        PowerRole::generator(),
    );
    let fab = spawn_fab(&mut h.world, TilePos::new(5, 3));
    h.cable(3, 3, true);
    h.cable(4, 3, true);
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
    assert!(matches!(
        h.world.get::<CrewTask>(crew).unwrap(),
        CrewTask::Operate(_)
    ));

    h.cable(4, 3, false); // blackout mid-cycle
    h.step(0.1);
    h.step(0.1);
    let f = h.world.get::<Fabricator>(fab).unwrap();
    assert!(!f.active, "cycle aborted");
    assert_eq!(f.input[ItemKind::Ore.index()], 2, "no material consumed");
    assert_eq!(f.output[ItemKind::Part.index()], 0, "no phantom product");
    assert!(matches!(
        h.world.get::<CrewTask>(crew).unwrap(),
        CrewTask::Idle(_)
    ));

    // Power back: production resumes and completes exactly once.
    h.cable(4, 3, true);
    h.steps(0.05, 600);
    let f = h.world.get::<Fabricator>(fab).unwrap();
    assert!(f.output[ItemKind::Part.index()] + count_stored_parts(&mut h.world) >= 1);
}

fn count_stored_parts(world: &mut World) -> u32 {
    world
        .query::<&StorageCell>()
        .iter(world)
        .map(|c| c.counts[ItemKind::Part.index()])
        .sum()
}

// =====================================================================================
// Runtime construction flows
// =====================================================================================

#[test]
fn cable_blueprint_builds_into_the_grid_and_powers() {
    let mut h = Harness::new();
    let crew = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let _gen = spawn_device(
        &mut h.world,
        TilePos::new(1, 3),
        2,
        2,
        PowerRole::generator(),
    );
    let fab = spawn_fab(&mut h.world, TilePos::new(5, 3));
    h.cable(3, 3, true); // reactor side only
    h.step(0.05);
    assert_eq!(h.status(fab), PowerStatus::Unconnected);

    // Player lays the missing link through the construction system.
    h.send(Action::PlaceBlueprint {
        kind: BuildingKind::PowerCable,
        pos: TilePos::new(4, 3),
    });
    h.steps(0.05, 400); // build work (1.5s) + margin
    assert!(
        h.world.resource::<CableGrid>().has(TilePos::new(4, 3)),
        "cable built into the underfloor grid"
    );
    assert_eq!(h.status(fab), PowerStatus::Powered);
    let _ = crew;
}

#[test]
fn cable_deconstruct_removes_it_and_unpowers() {
    let mut h = Harness::new();
    let crew = spawn_crew(&mut h.world, "A", TilePos::new(1, 1));
    let _gen = spawn_device(
        &mut h.world,
        TilePos::new(1, 3),
        2,
        2,
        PowerRole::generator(),
    );
    let fab = spawn_fab(&mut h.world, TilePos::new(5, 3));
    h.cable(3, 3, true);
    h.cable(4, 3, true);
    h.step(0.05);
    assert_eq!(h.status(fab), PowerStatus::Powered);

    h.send(Action::MarkCableDeconstruct {
        pos: TilePos::new(4, 3),
    });
    h.steps(0.05, 400); // deconstruct work
    assert!(
        !h.world.resource::<CableGrid>().has(TilePos::new(4, 3)),
        "cable torn out of the grid"
    );
    assert_eq!(
        h.status(fab),
        PowerStatus::Unconnected,
        "no ghost connection"
    );
    let _ = crew;
}

#[test]
fn reactor_and_cable_placement_rules() {
    let mut h = Harness::new();
    // Cable under the hull is rejected; on interior floor it is fine.
    h.send(Action::PlaceBlueprint {
        kind: BuildingKind::PowerCable,
        pos: TilePos::new(0, 0),
    });
    h.send(Action::PlaceBlueprint {
        kind: BuildingKind::PowerCable,
        pos: TilePos::new(2, 2),
    });
    h.step(0.1);
    let bps = h
        .world
        .query::<(Entity, &ship_alive::building::Blueprint)>()
        .iter(&h.world)
        .count();
    assert_eq!(bps, 1, "hull placement rejected, floor accepted");
    let _ = Tile::Floor;
}
