//! Slice "perf round 2" benchmarks for the jobs/path layer: A* throughput,
//! the idle work-scan under entity load, and end-to-end haul churn
//! (claim + walk + deliver). Release-oriented canaries — the printed rates
//! are the comparison baselines, the asserts only catch order-of-magnitude
//! regressions.

use bevy::ecs::schedule::Schedule;
use bevy::ecs::world::World;
use bevy::prelude::*;
use ship_alive::building::{Blueprint, Building, BuildingKind, Footprint};
use ship_alive::crew::{Crew, CrewTask, Movement};
use ship_alive::items::{Item, ItemKind, MarkedForHaul};
use ship_alive::jobs::{self, Action};
use ship_alive::log::EventLog;
use ship_alive::map::{ShipMap, TilePos};
use ship_alive::production::Fabricator;
use ship_alive::storage::StorageCell;

fn big_open_map(n: i32) -> (World, Schedule) {
    let rows: Vec<String> = (0..n)
        .map(|y| {
            if y == 0 || y == n - 1 {
                "#".repeat(n as usize)
            } else {
                format!("#{}#", ".".repeat((n - 2) as usize))
            }
        })
        .collect();
    let layout: Vec<&str> = rows.iter().map(|s| s.as_str()).collect();
    let (map, _) = ShipMap::from_layout(&layout);
    let (w, h) = (map.width, map.height);
    let mut world = World::new();
    world.insert_resource(map);
    world.insert_resource(EventLog::default());
    world.insert_resource(ship_alive::stats::Stats::default());
    world.insert_resource(ship_alive::power::CableGrid::new(w, h));
    world.insert_resource(ship_alive::coolant::PipeGrid::new(w, h));
    world.insert_resource(ship_alive::coolant::WaterGrid::new(w, h));
    world.insert_resource(ship_alive::coolant::CoolantStats::default());
    world.insert_resource(ship_alive::power::PowerState::default());
    world.insert_resource(ship_alive::simtime::SimClock::default());
    world.insert_resource(ship_alive::loc::Lang::default());
    world.init_resource::<Events<Action>>();
    world.insert_resource({
        let map = world.resource::<ShipMap>();
        ship_alive::thermal::ThermalGrid::new(map)
    });
    let mut schedule = Schedule::default();
    schedule.add_systems((jobs::crew_task_system, jobs::crew_scan_system).chain());
    (world, schedule)
}

fn spawn_crew(world: &mut World, pos: TilePos) -> Entity {
    let mut crew = Crew::new("Bench", Color::WHITE);
    crew.next_scan = 0.0;
    world
        .spawn((pos, crew, CrewTask::default(), Movement::default()))
        .id()
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

fn advance(world: &mut World, dt: f32) {
    world
        .resource_mut::<ship_alive::simtime::SimClock>()
        .advance_sim(dt as f64 * ship_alive::simtime::BASE_SIM_RATE);
}

// ---- pre-optimization HashMap A* (faithful copy for in-process A/B) ----
mod legacy {
    use bevy::prelude::*;
    use ship_alive::map::{ShipMap, TilePos};
    use std::collections::{BinaryHeap, HashMap};

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct OpenNode {
        cost: u32,
        est_total: u32,
        pos: TilePos,
    }

    impl Ord for OpenNode {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            other
                .est_total
                .cmp(&self.est_total)
                .then(other.cost.cmp(&self.cost))
        }
    }

    impl PartialOrd for OpenNode {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }

    const DIRS: [(i32, i32); 8] = [
        (1, 0),
        (-1, 0),
        (0, 1),
        (0, -1),
        (1, 1),
        (1, -1),
        (-1, 1),
        (-1, -1),
    ];

    fn step_cost(a: TilePos, b: TilePos) -> u32 {
        if a.x != b.x && a.y != b.y {
            14
        } else {
            10
        }
    }

    fn octile_cost(a: TilePos, b: TilePos) -> u32 {
        let dx = (a.x - b.x).unsigned_abs();
        let dy = (a.y - b.y).unsigned_abs();
        10 * dx.max(dy) + 4 * dx.min(dy)
    }

    fn step_enterable(map: &ShipMap, from: TilePos, to: TilePos) -> bool {
        if !map.is_walkable(to) {
            return false;
        }
        if from.x != to.x && from.y != to.y {
            let side_a = TilePos::new(to.x, from.y);
            let side_b = TilePos::new(from.x, to.y);
            if !map.is_walkable(side_a) || !map.is_walkable(side_b) {
                return false;
            }
        }
        true
    }

    pub fn find_path_hashmap(map: &ShipMap, from: TilePos, to: TilePos) -> Option<Vec<TilePos>> {
        if from == to {
            return Some(Vec::new());
        }
        if !map.is_walkable(to) || !map.is_standable(from) {
            return None;
        }
        let heuristic = |p: TilePos| octile_cost(p, to);
        let mut open = BinaryHeap::new();
        let mut best_cost: HashMap<TilePos, u32> = HashMap::new();
        let mut came_from: HashMap<TilePos, TilePos> = HashMap::new();
        open.push(OpenNode {
            cost: 0,
            est_total: heuristic(from),
            pos: from,
        });
        best_cost.insert(from, 0);
        while let Some(OpenNode { cost, pos, .. }) = open.pop() {
            if pos == to {
                let mut path = vec![to];
                while let Some(&prev) = came_from.get(path.last().unwrap()) {
                    if prev == from {
                        break;
                    }
                    path.push(prev);
                }
                path.reverse();
                return Some(path);
            }
            if best_cost.get(&pos).is_some_and(|c| *c < cost) {
                continue;
            }
            for (dx, dy) in DIRS {
                let next = TilePos::new(pos.x + dx, pos.y + dy);
                if !step_enterable(map, pos, next) {
                    continue;
                }
                let next_cost = cost + step_cost(pos, next);
                if next_cost < *best_cost.get(&next).unwrap_or(&u32::MAX) {
                    best_cost.insert(next, next_cost);
                    came_from.insert(next, pos);
                    open.push(OpenNode {
                        cost: next_cost,
                        est_total: next_cost + heuristic(next),
                        pos: next,
                    });
                }
            }
        }
        None
    }
}

// =====================================================================================
// 1. A* throughput on a 128x128 open map (long corner-to-corner paths).
// =====================================================================================

#[test]
fn perf_astar_long_paths() {
    let (map, _) = {
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
        ShipMap::from_layout(&layout)
    };
    // Interleaved batches of the dense and legacy (HashMap) implementations
    // so CPU-frequency drift hits both equally.
    let n = 2000;
    let mut dense_ms = 0.0f32;
    let mut legacy_ms = 0.0f32;
    let mut total_len = 0usize;
    for batch in 0..5 {
        let t0 = std::time::Instant::now();
        for i in 0..n {
            let b = if (i + batch) % 2 == 0 {
                TilePos::new(125, 125)
            } else {
                TilePos::new(125, 2)
            };
            total_len += ship_alive::path::find_path(&map, TilePos::new(2, 2), b, |_| false)
                .expect("open map")
                .len();
        }
        dense_ms += t0.elapsed().as_secs_f32() * 1000.0;
        let t1 = std::time::Instant::now();
        for i in 0..n {
            let b = if (i + batch) % 2 == 0 {
                TilePos::new(125, 125)
            } else {
                TilePos::new(125, 2)
            };
            total_len += legacy::find_path_hashmap(&map, TilePos::new(2, 2), b)
                .expect("open map")
                .len();
        }
        legacy_ms += t1.elapsed().as_secs_f32() * 1000.0;
    }
    let total = 5 * n;
    println!(
        "PERF A* dense: {total} paths in {dense_ms:.0}ms ({:.0}/s) | legacy HashMap: {legacy_ms:.0}ms ({:.0}/s) | speedup {:.2}x | avg len {:.0}",
        total as f32 / (dense_ms / 1000.0),
        total as f32 / (legacy_ms / 1000.0),
        legacy_ms / dense_ms,
        total_len as f32 / (2 * total) as f32
    );
    assert!(dense_ms < 30000.0, "A* bench must finish");
}

// =====================================================================================
// 2. Idle work-scan under load: 8 crews rescanning every step with 240
// marked ground items, 24 racks, 8 blueprints demanding materials and 4
// fabricators with empty inputs (worst-case auto-logistics demand scan).
// =====================================================================================

#[test]
fn perf_scan_under_entity_load() {
    let (mut world, mut schedule) = big_open_map(128);

    // Racks around the far corner.
    for i in 0..24 {
        spawn_rack(
            &mut world,
            TilePos::new(100 + (i % 8) * 3, 110 + (i / 8) * 3),
        );
    }
    // Marked items scattered in the west half (all claimable).
    for i in 0..240 {
        let x = 2 + (i % 40) * 2;
        let y = 2 + (i / 40) * 2;
        world
            .spawn((
                TilePos::new(x, y),
                Item {
                    kind: ItemKind::Crate,
                },
            ))
            .insert(MarkedForHaul);
    }
    // Blueprints needing parts (auto-demand pulls from racks/ground).
    for i in 0..8 {
        world.spawn((
            TilePos::new(60 + i * 4, 60),
            Footprint::new(60 + i * 4, 60, 1, 1),
            Blueprint {
                kind: BuildingKind::Wall,
                foot: Footprint::new(60 + i * 4, 60, 1, 1),
                delivered: [0, 0, 0],
                progress: 0.0,
            },
        ));
    }
    // Fabricators with orders and empty input buffers.
    for i in 0..4 {
        let pos = TilePos::new(60 + i * 5, 70);
        world.spawn((
            pos,
            Footprint::new(pos.x, pos.y, 2, 2),
            Building {
                kind: BuildingKind::Fabricator,
                foot: Footprint::new(pos.x, pos.y, 2, 2),
                demo_progress: 0.0,
            },
            Fabricator {
                order: Some(ship_alive::production::Order {
                    batches: 9,
                    repeat: false,
                }),
                ..Default::default()
            },
            ship_alive::power::PowerRole::consumer(20),
            ship_alive::power::PowerStatus::default(),
        ));
        let mut cables = world
            .remove_resource::<ship_alive::power::CableGrid>()
            .unwrap();
        for y in [pos.y, pos.y + 1] {
            for x in (pos.x - 2)..pos.x {
                cables.set(TilePos::new(x, y), true);
            }
        }
        world.insert_resource(cables);
    }
    for i in 0..8 {
        spawn_crew(&mut world, TilePos::new(2 + i, 4));
    }

    // Run the scan-only schedule. Crews claim work fast, so to keep the
    // worst-case load we reset their scans/tasks each iteration (fresh idle
    // rescans with the full entity set).
    let mut scan_only = Schedule::default();
    scan_only.add_systems(jobs::crew_scan_system);
    let steps = 1000;
    let t0 = std::time::Instant::now();
    for i in 0..steps {
        advance(&mut world, 1.0 / 60.0);
        if i % 3 == 0 {
            // Keep everyone idle and rescanning (claim discipline is covered
            // by the behavior tests; this measures the scan cost itself).
            let mut q = world.query_filtered::<&mut CrewTask, With<Crew>>();
            for mut t in q.iter_mut(&mut world) {
                *t = CrewTask::default();
            }
            let mut q2 = world.query_filtered::<&mut Crew, With<Crew>>();
            for mut c in q2.iter_mut(&mut world) {
                c.next_scan = 0.0;
            }
        }
        scan_only.run(&mut world);
    }
    let dt = t0.elapsed().as_secs_f32();
    println!(
        "PERF SCAN: {steps} loaded scans in {dt:.3}s ({:.0} scans/s, 240 items/24 racks/12 demands)",
        steps as f32 / dt
    );
    assert!(dt < 30.0, "scan bench must finish, took {dt:.2}s");
    let _ = &mut schedule;
}

// =====================================================================================
// 3. End-to-end haul churn: 8 crews claiming far items and delivering to
// far racks on a 128x128 map (claim + A* + walk + store, repeated).
// =====================================================================================

#[test]
fn perf_haul_churn_end_to_end() {
    let (mut world, mut schedule) = big_open_map(128);
    for i in 0..16 {
        spawn_rack(&mut world, TilePos::new(60 + (i % 4) * 3, 30 + (i / 4) * 3));
    }
    let mut item_seq: usize = 0;
    let spawn_wave = |world: &mut World, item_seq: &mut usize| {
        for i in 0..80 {
            let x = 2 + (i % 20) * 2;
            let y = 2 + (i / 20) * 2;
            world
                .spawn((
                    TilePos::new(x, y),
                    Item {
                        kind: ItemKind::Crate,
                    },
                ))
                .insert(MarkedForHaul);
            *item_seq += 1;
        }
    };
    spawn_wave(&mut world, &mut item_seq);
    for i in 0..8 {
        spawn_crew(&mut world, TilePos::new(30 + i, 20));
    }

    let mut movement = Schedule::default();
    movement.add_systems(ship_alive::movement::movement_system);

    let steps = 20000;
    let t0 = std::time::Instant::now();
    for _ in 0..steps {
        advance(&mut world, 1.0 / 60.0);
        schedule.run(&mut world);
        movement.run(&mut world);
        world.resource_mut::<Events<Action>>().update();
    }
    let dt = t0.elapsed().as_secs_f32();
    let stored: u32 = world
        .query_filtered::<&StorageCell, ()>()
        .iter(&world)
        .map(|c| c.stored())
        .sum();
    let hauled = world.resource::<ship_alive::stats::Stats>().hauls_done;
    println!(
        "PERF CHURN: {steps} steps in {dt:.2}s ({:.0} steps/s), hauled {hauled}, stored {stored}",
        steps as f32 / dt
    );
    assert!(
        stored > 8,
        "the run must actually move cargo (stored {stored})"
    );
    assert!(dt < 120.0, "churn bench must finish, took {dt:.2}s");
}
