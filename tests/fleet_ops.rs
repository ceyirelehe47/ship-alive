//! Faithful headless reproduction of scenario F: full starter ship (racks,
//! fabricator, reactor, cables, items from MAP_LAYOUT), F's actions, then
//! periodic part-flow snapshots to find where materials vanish.

use bevy::ecs::schedule::Schedule;
use bevy::ecs::world::World;
use bevy::prelude::*;
use ship_alive::building::{Building, BuildingKind, Footprint};
use ship_alive::crew::Crew;
use ship_alive::items::{Item, ItemKind};
use ship_alive::jobs::{self, Action};
use ship_alive::log::EventLog;
use ship_alive::map::{ShipMap, SpawnReq, TilePos, MAP_LAYOUT};
use ship_alive::power::{CableGrid, PowerRole, PowerStatus, FABRICATOR_DEMAND};
use ship_alive::storage::StorageCell;

const ROSTER: [(&str, [f32; 3]); 4] = [
    ("Ava", [0.98, 0.45, 0.42]),
    ("Rex", [0.45, 0.65, 0.98]),
    ("Mio", [0.50, 0.92, 0.55]),
    ("Zed", [0.80, 0.55, 0.95]),
];

fn setup_full_world() -> World {
    let mut world = World::new();
    let (map, spawns) = ShipMap::from_layout(&MAP_LAYOUT);
    let (w, h) = (map.width, map.height);
    world.insert_resource(map);
    world.insert_resource(EventLog::default());
    world.insert_resource(ship_alive::stats::Stats::default());
    let mut cables = CableGrid::new(w, h);
    let mut crew_idx = 0;
    for req in spawns {
        match req {
            SpawnReq::Crew { pos } => {
                let (name, tint) = ROSTER[crew_idx.min(3)];
                crew_idx += 1;
                let mut c = Crew::new(name, Color::srgb(tint[0], tint[1], tint[2]));
                c.next_scan = 0.05 * crew_idx as f64;
                world.spawn((
                    pos,
                    c,
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
                ));
            }
            SpawnReq::Cable { pos } => {
                cables.set(pos, true);
            }
            SpawnReq::Item { pos, kind } => {
                world.spawn((pos, Item { kind }));
            }
        }
    }
    world.insert_resource(cables);
    world.insert_resource(ship_alive::power::PowerState::default());
    world.insert_resource(ship_alive::simtime::SimClock::default());
    world.init_resource::<Events<Action>>();
    world
}

#[test]
fn fab_fleet_builds_with_material_conservation() {
    let mut world = setup_full_world();
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

    // F's script.
    let mut ev = world.resource_mut::<Events<Action>>();
    for pos in [(12, 10), (19, 10), (17, 14), (19, 14), (17, 16), (12, 12)] {
        ev.send(Action::PlaceBlueprint {
            kind: BuildingKind::Fabricator,
            pos: TilePos::new(pos.0, pos.1),
        });
    }
    for x in 16..=20 {
        ev.send(Action::PlaceBlueprint {
            kind: BuildingKind::PowerCable,
            pos: TilePos::new(x, 14),
        });
    }
    for y in [11, 12, 13, 15, 16, 17] {
        ev.send(Action::PlaceBlueprint {
            kind: BuildingKind::PowerCable,
            pos: TilePos::new(17, y),
        });
        ev.send(Action::PlaceBlueprint {
            kind: BuildingKind::PowerCable,
            pos: TilePos::new(19, y),
        });
    }
    for p in [(13, 12), (13, 13), (14, 13)] {
        ev.send(Action::PlaceBlueprint {
            kind: BuildingKind::PowerCable,
            pos: TilePos::new(p.0, p.1),
        });
    }
    for _ in 0..20 {
        ev.send(Action::SpawnItem {
            kind: ItemKind::Part,
        });
    }

    for i in 0..8000 {
        world
            .resource_mut::<ship_alive::simtime::SimClock>()
            .advance_sim(0.05 * ship_alive::simtime::BASE_SIM_RATE);
        world.resource_mut::<Events<Action>>().update();
        schedule.run(&mut world);
        if i % 1000 == 0 {
            let t = world.resource::<ship_alive::simtime::SimClock>().now() as f32;
            let carried = world
                .query_filtered::<&Item, With<ship_alive::items::CarriedBy>>()
                .iter(&world)
                .filter(|i| i.kind == ItemKind::Part)
                .count();
            let ground = world
                .query::<&Item>()
                .iter(&world)
                .filter(|i| i.kind == ItemKind::Part)
                .count();
            let stored: u32 = world
                .query::<&StorageCell>()
                .iter(&world)
                .map(|c| c.counts[ItemKind::Part.index()])
                .sum();
            let mut bq = world.query::<(Entity, &ship_alive::building::Blueprint)>();
            let mat: Vec<String> = bq
                .iter(&world)
                .filter(|(_, bp)| bp.kind == BuildingKind::Fabricator)
                .map(|(e, bp)| {
                    format!(
                        "{e:?}@({},{}) {}",
                        bp.foot.x,
                        bp.foot.y,
                        bp.materials_label()
                    )
                })
                .collect();
            let built = world
                .query::<&Building>()
                .iter(&world)
                .filter(|b| b.kind == BuildingKind::Fabricator)
                .count();
            let mut cq = world.query::<(
                &Crew,
                &ship_alive::crew::CrewTask,
                &TilePos,
                &ship_alive::crew::Movement,
            )>();
            let crew_states: Vec<String> = cq
                .iter(&world)
                .map(|(c, t, pos, mov)| {
                    let task_desc = match t {
                        ship_alive::crew::CrewTask::Idle(cause) => cause.label(),
                        ship_alive::crew::CrewTask::Haul(j) => format!(
                            "haul({:?},{:?},item {:?})",
                            j.phase,
                            j.dest,
                            world.get::<Item>(j.item).map(|i| i.kind)
                        ),
                        ship_alive::crew::CrewTask::Build(_) => "build".into(),
                        ship_alive::crew::CrewTask::Deconstruct(_) => "demo".into(),
                        ship_alive::crew::CrewTask::Operate(_) => "op".into(),
                    };
                    format!(
                        "{}@({},{})p{}:[{}]",
                        c.name,
                        pos.x,
                        pos.y,
                        mov.path.len(),
                        task_desc
                    )
                })
                .collect();
            println!(
                "t={t:.0} fabs={built} parts[carried={carried} ground={ground} stored={stored}] bp={mat:?} crew={crew_states:?}"
            );
        }
    }
    for e in world
        .get_resource::<EventLog>()
        .unwrap()
        .entries
        .iter()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .iter()
        .rev()
    {
        println!("LOG {:?} {}", e.kind, e.text);
    }

    // Direct connectivity test: can anyone path from the S-rack area to each
    // fab blueprint's interaction tiles?
    let map = world.resource::<ShipMap>();
    for (pos, name) in [
        (TilePos::new(12, 12), "NW(12,12)"),
        (TilePos::new(17, 10), "E1(17,10)"),
        (TilePos::new(17, 14), "E2(17,14)"),
    ] {
        let foot = Footprint::new(pos.x, pos.y, 2, 2);
        let reach = ship_alive::building::path_to_interaction(map, TilePos::new(27, 14), &foot);
        let walkable: Vec<(i32, i32)> = foot.tiles().map(|t| (t.x, t.y)).collect();
        println!(
            "PATH {name} tiles={walkable:?} blueprint_tiles_walkable={:?} reachable={}",
            foot.tiles().all(|t| map.is_walkable(t)),
            reach.is_some()
        );
    }

    // The whole fleet must share ONE power network with the reactor:
    // 7 fabs x 20 PU = 140 demand vs 100 generation -> deterministic shed.
    let nets = world
        .resource::<ship_alive::power::PowerState>()
        .networks
        .clone();
    assert_eq!(nets.len(), 1, "spur bridges into the main grid: {nets:?}");
    assert_eq!(nets[0].generation, 100);
    assert_eq!(nets[0].demand, 140);
    assert_eq!(nets[0].served, 100);

    // All six fabricators built, nothing left mid-flight.
    let built = world
        .query::<&Building>()
        .iter(&world)
        .filter(|b| b.kind == BuildingKind::Fabricator)
        .count();
    assert_eq!(built, 7, "starter + six new fabricators");
    let carried = world
        .query_filtered::<&Item, With<ship_alive::items::CarriedBy>>()
        .iter(&world)
        .filter(|i| i.kind == ItemKind::Part)
        .count();
    assert_eq!(carried, 0, "no items stuck in carried limbo");
    // Conservation: 9 ground + 8 rack + 20 spawned = 37 parts total.
    let ground = world
        .query::<&Item>()
        .iter(&world)
        .filter(|i| i.kind == ItemKind::Part)
        .count();
    let stored: u32 = world
        .query::<&StorageCell>()
        .iter(&world)
        .map(|c| c.counts[ItemKind::Part.index()])
        .sum();
    assert_eq!(
        ground as u32 + stored + 24,
        37,
        "24 parts consumed by builds, rest intact"
    );
}

/// A blueprint sealed behind walls must not attract supply hauls: without the
/// claim-time reachability check this loops forever (rack -> carry ->
/// convert-to-storage -> rack).
#[test]
fn sealed_blueprint_does_not_pump_storage() {
    let mut world = setup_full_world();
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

    // Seal FABRICATION's only door with a wall blueprint that is instantly
    // fully supplied, then put a fabricator blueprint inside the sealed room.
    let mut map = world.remove_resource::<ShipMap>().unwrap();
    map.set_tile(TilePos::new(17, 9), ship_alive::map::Tile::BuiltWall);
    world.insert_resource(map);

    let mut ev = world.resource_mut::<Events<Action>>();
    ev.send(Action::PlaceBlueprint {
        kind: BuildingKind::Fabricator,
        pos: TilePos::new(17, 12),
    });

    for _ in 0..2000 {
        world
            .resource_mut::<ship_alive::simtime::SimClock>()
            .advance_sim(0.05 * ship_alive::simtime::BASE_SIM_RATE);
        world.resource_mut::<Events<Action>>().update();
        schedule.run(&mut world);
    }

    // The blueprint exists but must have zero delivered materials and the
    // world must be quiet: no rack stock churned into ground items.
    let mut bq = world.query::<(Entity, &ship_alive::building::Blueprint)>();
    let bps: Vec<(Entity, &ship_alive::building::Blueprint)> = bq
        .iter(&world)
        .filter(|(_, bp)| bp.kind == BuildingKind::Fabricator)
        .collect();
    assert_eq!(bps.len(), 1, "sealed blueprint still placed");
    assert_eq!(
        bps[0].1.delivered[ItemKind::Part.index()],
        0,
        "no materials delivered"
    );
    let stored_after: u32 = world
        .query::<&StorageCell>()
        .iter(&world)
        .map(|c| c.counts[ItemKind::Part.index()])
        .sum();
    assert!(
        stored_after >= 8,
        "rack parts not drained by a pointless pump: {stored_after}"
    );
}
