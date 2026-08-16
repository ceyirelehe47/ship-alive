//! Headless integration tests for Slice 4: airtight compartments & doors.
//! Runs the real door system / compartment cache / thermal seal on bare
//! bevy_ecs worlds built from small hand-authored layouts (`D` = door).

use bevy::ecs::schedule::Schedule;
use bevy::ecs::world::World;
use bevy::prelude::*;
use ship_alive::airtight::{
    boundary, door_axis, Boundary, Compartments, Door, DoorAxis, DoorDemand, DoorMode, DoorPhase,
    NO_REGION,
};
use ship_alive::crew::{Crew, CrewTask, Movement};
use ship_alive::map::{ShipMap, SpawnReq, Tile, TilePos};
use ship_alive::simtime::SimClock;
use ship_alive::thermal::{ThermalGrid, AMBIENT_START};

/// Two rooms split by a horizontal wall with one door (walls east+west of
/// the leaf, passage north-south).
const TWO_ROOMS: [&str; 5] = ["#######", "#.....#", "###D###", "#.....#", "#######"];

fn setup(rows: &[&str]) -> World {
    let (map, spawns) = ShipMap::from_layout(rows);
    let tile_count = (map.width * map.height) as usize;
    let mut world = World::new();
    let thermal = ThermalGrid::new(&map);
    let comps = Compartments::rebuild(&map);
    world.insert_resource(map);
    world.insert_resource(thermal);
    world.insert_resource(comps);
    world.insert_resource(DoorDemand::default());
    world.insert_resource(SimClock::default());
    world.insert_resource(ship_alive::log::EventLog::default());
    world.insert_resource(ship_alive::power::PowerState::default());
    world.insert_resource(ship_alive::thermal::ThermalStats::default());
    world.insert_resource(ship_alive::thermal::DeviceTiles::sized(tile_count));
    for req in spawns {
        if let SpawnReq::Door { pos } = req {
            let axis = door_axis(world.resource::<ShipMap>(), pos).unwrap();
            world.spawn((pos, Door::new(axis)));
        }
    }
    world
}

fn door_schedule(with_movement: bool) -> Schedule {
    let mut s = Schedule::default();
    if with_movement {
        s.add_systems(
            (
                ship_alive::airtight::door_system,
                ship_alive::movement::movement_system,
            )
                .chain(),
        );
    } else {
        s.add_systems((ship_alive::airtight::door_system,));
    }
    s
}

fn thermal_schedule() -> Schedule {
    let mut s = Schedule::default();
    s.add_systems((ship_alive::thermal::thermal_air_system,));
    s
}

fn step(world: &mut World, schedule: &mut Schedule, sim_secs: f64) {
    world.resource_mut::<SimClock>().advance_sim(sim_secs);
    schedule.run(world);
}

fn door_entity(world: &mut World) -> Entity {
    let mut q = world.query::<(Entity, &TilePos, &Door)>();
    q.iter(world).next().map(|(e, _, _)| e).unwrap()
}

fn door_at(world: &mut World, x: i32, y: i32) -> Door {
    let mut q = world.query::<(Entity, &TilePos, &Door)>();
    q.iter(world)
        .find(|(_, p, _)| p.x == x && p.y == y)
        .map(|(_, _, d)| Door {
            mode: d.mode,
            phase: d.phase,
            progress: d.progress,
            hold_until: d.hold_until,
            axis: d.axis,
            cycles: d.cycles,
        })
        .unwrap()
}

fn demand(world: &mut World, p: TilePos) {
    world.resource_mut::<DoorDemand>().0.insert(p);
}

// =====================================================================================
// Structural compartments
// =====================================================================================

#[test]
fn two_rooms_two_compartments_one_portal() {
    let w = setup(&TWO_ROOMS);
    let comps = w.resource::<Compartments>();
    assert_eq!(comps.regions.len(), 2, "rooms a and b are separate");
    assert_eq!(comps.doors.len(), 1, "exactly one door portal");
    let portal = &comps.doors[0];
    assert_eq!(portal.axis, DoorAxis::Ns);
    assert_ne!(portal.side_a, portal.side_b);
    // Both sides resolve to real regions and are air-distinct while closed.
    assert_ne!(portal.side_a, NO_REGION);
    assert_ne!(portal.side_b, NO_REGION);
    assert_eq!(comps.air_groups, 2, "closed door separates air");
    // Region membership is by flood fill, not labels.
    let (ra, rb) = (
        comps.region_at(TilePos::new(3, 1)),
        comps.region_at(TilePos::new(3, 3)),
    );
    assert_ne!(ra, rb);
    assert_eq!(comps.region_at(TilePos::new(1, 1)), ra);
    assert_eq!(comps.region_at(TilePos::new(5, 3)), rb);
}

#[test]
fn starter_ship_boots_six_sealed_compartments() {
    let (map, spawns) = ShipMap::from_layout(&ship_alive::map::MAP_LAYOUT);
    let doors: Vec<TilePos> = spawns
        .iter()
        .filter_map(|s| match s {
            SpawnReq::Door { pos } => Some(*pos),
            _ => None,
        })
        .collect();
    assert_eq!(doors.len(), 5);
    let comps = Compartments::rebuild(&map);
    // 6 crew-facing compartments + the sealed scenario-C pocket at (3,16).
    assert_eq!(comps.regions.len(), 7, "6 rooms + the sealed pocket");
    assert_eq!(comps.sealed_count(), 7, "all sealed at boot");
    assert_eq!(comps.exposed_count(), 0, "nothing exposed to space");
    assert_eq!(comps.doors.len(), 5, "five portals");
    assert_eq!(
        comps.air_groups, 7,
        "all doors closed at boot: no air links"
    );
    let _ = &map;
}

#[test]
fn sealed_pocket_is_its_own_region() {
    let mut w = setup(&TWO_ROOMS);
    // Wall across room a's single row: the far cells become a sealed pocket.
    w.resource_mut::<ShipMap>()
        .set_tile(TilePos::new(2, 1), Tile::BuiltWall);
    let fresh = Compartments::rebuild(w.resource::<ShipMap>());
    assert_eq!(fresh.regions.len(), 3, "pocket sealed off by the wall");
}

#[test]
fn exposed_region_detected_on_synthetic_hull_gap() {
    // Synthetic map whose border row itself carries floor: the interior
    // region touches out-of-bounds and must be EXPOSED.
    let (map2, _) =
        ShipMap::from_layout(&["######", "#....#", "#.##.#", "#.....", "#....#", "######"]);
    let comps = Compartments::rebuild(&map2);
    assert_eq!(comps.regions.len(), 1, "interior is one connected region");
    assert_eq!(comps.exposed_count(), 1, "border floor tile vents to space");
    assert_eq!(comps.sealed_count(), 0);
    // The same layout fully walled stays sealed.
    let (map3, _) =
        ShipMap::from_layout(&["######", "#....#", "#.##.#", "#....#", "#....#", "######"]);
    let comps3 = Compartments::rebuild(&map3);
    assert_eq!(comps3.exposed_count(), 0);
    assert!(comps3.sealed_count() >= 1);
}

#[test]
fn structural_split_on_wall_build() {
    let mut w = setup(&["#######", "#.....#", "###D###", "#.....#", "#######"]);
    let before = w.resource::<Compartments>().regions.len();
    w.resource_mut::<ShipMap>()
        .set_tile(TilePos::new(3, 3), Tile::BuiltWall);
    let fresh = Compartments::rebuild(w.resource::<ShipMap>());
    assert_eq!(
        fresh.regions.len(),
        before + 1,
        "wall splits the lower room"
    );
}

#[test]
fn structural_merge_on_wall_removal() {
    let mut w = setup(&TWO_ROOMS);
    // Split room b, then tear the splitter back out.
    w.resource_mut::<ShipMap>()
        .set_tile(TilePos::new(3, 3), Tile::BuiltWall);
    let split = Compartments::rebuild(w.resource::<ShipMap>());
    assert_eq!(split.regions.len(), 3);
    w.resource_mut::<ShipMap>()
        .set_tile(TilePos::new(3, 3), Tile::Floor);
    let merged = Compartments::rebuild(w.resource::<ShipMap>());
    assert_eq!(merged.regions.len(), 2, "compartments merge again");
}

#[test]
fn portal_created_and_removed_with_door_tile() {
    let mut w = setup(&TWO_ROOMS);
    assert_eq!(w.resource::<Compartments>().doors.len(), 1);
    // Remove the door: portal disappears, the two rooms merge.
    w.resource_mut::<ShipMap>()
        .set_tile(TilePos::new(3, 2), Tile::Floor);
    let fresh = Compartments::rebuild(w.resource::<ShipMap>());
    assert_eq!(fresh.doors.len(), 0, "no ghost portal");
    assert_eq!(fresh.regions.len(), 1, "rooms permanently merged");
    assert_eq!(fresh.air_groups, 1);
    // Put it back.
    w.resource_mut::<ShipMap>()
        .set_tile(TilePos::new(3, 2), Tile::Door);
    let again = Compartments::rebuild(w.resource::<ShipMap>());
    assert_eq!(again.doors.len(), 1);
    assert_eq!(again.regions.len(), 2);
}

// =====================================================================================
// Airtight connectivity
// =====================================================================================

#[test]
fn air_groups_follow_door_seal() {
    let mut w = setup(&TWO_ROOMS);
    let mut sched = door_schedule(false);
    // Closed: separate groups.
    assert_eq!(w.resource::<Compartments>().air_groups, 2);
    // Open the door (demand it): groups merge.
    demand(&mut w, TilePos::new(3, 2));
    for _ in 0..25 {
        step(&mut w, &mut sched, 1.0);
    }
    assert!(door_at(&mut w, 3, 2).progress >= 1.0);
    assert_eq!(
        w.resource::<Compartments>().air_groups,
        1,
        "open door joins air"
    );
    // Hold expires, door closes: groups split again.
    for _ in 0..60 {
        step(&mut w, &mut sched, 1.0);
    }
    assert!(door_at(&mut w, 3, 2).progress < 1.0);
    assert_eq!(w.resource::<Compartments>().air_groups, 2);
}

#[test]
fn boundary_query_reflects_walls_doors_and_air() {
    let mut w = setup(&TWO_ROOMS);
    let mut sched = door_schedule(false);
    demand(&mut w, TilePos::new(3, 2));
    for _ in 0..25 {
        step(&mut w, &mut sched, 1.0);
    }
    let map = w.resource::<ShipMap>().clone();
    // Open door: door↔neighbour exchange allowed.
    assert_eq!(
        boundary(&map, TilePos::new(3, 1), TilePos::new(3, 2)),
        Boundary::Open
    );
    // Wall: blocked.
    assert_eq!(
        boundary(&map, TilePos::new(1, 1), TilePos::new(0, 1)),
        Boundary::Blocked
    );
    // Same room: open.
    assert_eq!(
        boundary(&map, TilePos::new(1, 1), TilePos::new(2, 1)),
        Boundary::Open
    );
    // Non-adjacent: blocked by contract.
    assert_eq!(
        boundary(&map, TilePos::new(1, 1), TilePos::new(5, 3)),
        Boundary::Blocked
    );
}

// =====================================================================================
// Door runtime
// =====================================================================================

#[test]
fn auto_door_full_passage_cycle() {
    let mut w = setup(&TWO_ROOMS);
    let mut sched = door_schedule(false);
    // Closed and sealed at boot.
    assert_eq!(door_at(&mut w, 3, 2).phase, DoorPhase::Closed);
    assert!(w
        .resource::<ThermalGrid>()
        .door_sealed_at(TilePos::new(3, 2)));
    // Demand: opens fully, then (idle) closes again.
    demand(&mut w, TilePos::new(3, 2));
    let mut seen = Vec::new();
    for _ in 0..25 {
        step(&mut w, &mut sched, 1.0);
        seen.push(door_at(&mut w, 3, 2).phase);
    }
    assert!(seen.contains(&DoorPhase::Opening));
    assert_eq!(*seen.last().unwrap(), DoorPhase::Open);
    assert!(!w
        .resource::<ThermalGrid>()
        .door_sealed_at(TilePos::new(3, 2)));
    for _ in 0..60 {
        step(&mut w, &mut sched, 1.0);
    }
    let d = door_at(&mut w, 3, 2);
    assert_eq!(d.phase, DoorPhase::Closed, "auto-closed after hold expired");
    assert_eq!(d.cycles, 1, "one completed cycle");
    assert!(w
        .resource::<ThermalGrid>()
        .door_sealed_at(TilePos::new(3, 2)));
}

#[test]
fn hold_open_mode_stays_open() {
    let mut w = setup(&TWO_ROOMS);
    let mut sched = door_schedule(false);
    let e = door_entity(&mut w);
    w.get_mut::<Door>(e).unwrap().mode = DoorMode::HoldOpen;
    for _ in 0..40 {
        step(&mut w, &mut sched, 1.0);
    }
    let d = door_at(&mut w, 3, 2);
    assert_eq!(d.phase, DoorPhase::Open);
    assert_eq!(d.cycles, 0, "never closes while held");
    assert_eq!(w.resource::<Compartments>().air_groups, 1);
}

#[test]
fn lock_closed_blocks_and_never_reopens() {
    let mut w = setup(&TWO_ROOMS);
    let mut sched = door_schedule(false);
    let e = door_entity(&mut w);
    w.get_mut::<Door>(e).unwrap().mode = DoorMode::LockClosed;
    // Even with demands, a locked door never opens.
    for _ in 0..50 {
        demand(&mut w, TilePos::new(3, 2));
        step(&mut w, &mut sched, 1.0);
    }
    assert_eq!(door_at(&mut w, 3, 2).progress, 0.0);
    assert!(!w.resource::<ShipMap>().is_walkable(TilePos::new(3, 2)));
    assert_eq!(w.resource::<Compartments>().air_groups, 2);
}

#[test]
fn locked_door_closing_waits_for_occupant() {
    let mut w = setup(&TWO_ROOMS);
    let mut sched = door_schedule(false);
    // Open the door first.
    demand(&mut w, TilePos::new(3, 2));
    for _ in 0..25 {
        step(&mut w, &mut sched, 1.0);
    }
    // A crew member stands in the doorway while it is told to lock.
    let crew = w
        .spawn((
            TilePos::new(3, 2),
            Crew::new("A", Color::WHITE),
            CrewTask::default(),
            Movement::default(),
        ))
        .id();
    let e = door_entity(&mut w);
    w.get_mut::<Door>(e).unwrap().mode = DoorMode::LockClosed;
    for _ in 0..40 {
        step(&mut w, &mut sched, 1.0);
    }
    assert!(
        door_at(&mut w, 3, 2).progress >= 0.99,
        "door must not close onto the crew member"
    );
    // The crew steps out of the doorway; now it may close.
    let mut q = w.query::<&mut TilePos>();
    *q.get_mut(&mut w, crew).unwrap() = TilePos::new(3, 3);
    drop(q);
    for _ in 0..40 {
        step(&mut w, &mut sched, 1.0);
    }
    assert_eq!(door_at(&mut w, 3, 2).progress, 0.0, "closed once clear");
}

#[test]
fn multi_crew_stream_does_not_flap() {
    let mut w = setup(&TWO_ROOMS);
    let mut sched = door_schedule(false);
    // Simulate a stream: one demand every 20 sim s for a long while.
    let mut openings = 0u32;
    let mut last_progress = 0.0f32;
    for i in 0..600 {
        if i % 20 == 0 {
            demand(&mut w, TilePos::new(3, 2));
        }
        step(&mut w, &mut sched, 1.0);
        let p = door_at(&mut w, 3, 2).progress;
        if last_progress < 1.0 && p >= 1.0 {
            openings += 1;
        }
        last_progress = p;
    }
    let d = door_at(&mut w, 3, 2);
    // 30 demands crossing in one continuous stream: one opening episode (a
    // couple at most — the stream is far denser than the hold window).
    assert!(
        openings <= 2,
        "stream must keep the door open, got {openings} opening episodes"
    );
    assert!(d.cycles <= 1, "no open/close chatter: cycles={}", d.cycles);
    // After the stream ends the door finally closes.
    for _ in 0..80 {
        step(&mut w, &mut sched, 1.0);
    }
    assert_eq!(door_at(&mut w, 3, 2).phase, DoorPhase::Closed);
}

#[test]
fn movement_demands_and_waits_for_closed_auto_door() {
    let mut w = setup(&TWO_ROOMS);
    let mut sched = door_schedule(true);
    let crew = w
        .spawn((
            TilePos::new(3, 1),
            Crew::new("A", Color::WHITE),
            CrewTask::default(),
            Movement {
                path: vec![TilePos::new(3, 2), TilePos::new(3, 3)],
                ..default()
            },
        ))
        .id();
    // A handful of steps: the crew must NOT step onto the closed door yet,
    // must register a demand, and must not escalate any avoidance clock.
    for _ in 0..5 {
        step(&mut w, &mut sched, 1.0);
    }
    let pos = *w.get::<TilePos>(crew).unwrap();
    assert_eq!(pos, TilePos::new(3, 1), "crew waits for the door");
    let mov = w.get::<Movement>(crew).unwrap();
    assert_eq!(mov.blocked_for, 0.0, "door wait is not congestion");
    assert_eq!(mov.stuck_for, 0.0, "watchdog frozen while waiting");
    assert!(!mov.passing_through);
    assert!(door_at(&mut w, 3, 2).progress > 0.0, "demand opened it");
    // After the door fully opens the crew walks through (~20 sim s/tile).
    for _ in 0..120 {
        step(&mut w, &mut sched, 1.0);
    }
    let pos = *w.get::<TilePos>(crew).unwrap();
    assert_eq!(pos, TilePos::new(3, 3), "crew passed through the doorway");
}

#[test]
fn movement_clears_path_when_door_locks_mid_route() {
    let mut w = setup(&TWO_ROOMS);
    let mut sched = door_schedule(true);
    let crew = w
        .spawn((
            TilePos::new(3, 1),
            Crew::new("A", Color::WHITE),
            CrewTask::default(),
            Movement {
                path: vec![TilePos::new(3, 2), TilePos::new(3, 3)],
                ..default()
            },
        ))
        .id();
    let e = door_entity(&mut w);
    w.get_mut::<Door>(e).unwrap().mode = DoorMode::LockClosed;
    step(&mut w, &mut sched, 1.0);
    assert!(
        w.get::<Movement>(crew).unwrap().path.is_empty(),
        "stale plan through a locked door is dropped"
    );
    assert_eq!(*w.get::<TilePos>(crew).unwrap(), TilePos::new(3, 1));
}

#[test]
fn pause_freezes_door_progress_and_timers() {
    let mut w = setup(&TWO_ROOMS);
    let mut sched = door_schedule(false);
    demand(&mut w, TilePos::new(3, 2));
    for _ in 0..10 {
        step(&mut w, &mut sched, 1.0);
    }
    let mid = door_at(&mut w, 3, 2).progress;
    assert!(mid > 0.0 && mid < 1.0);
    // Paused: zero sim advance, nothing moves.
    for _ in 0..30 {
        step(&mut w, &mut sched, 0.0);
    }
    assert_eq!(door_at(&mut w, 3, 2).progress, mid);
}

#[test]
fn door_rate_depends_only_on_sim_time_integral() {
    // "1x vs 4x": stepping the same total sim seconds reaches the same door
    // state, whether in 1 s chunks or 4 s chunks.
    let run = |chunk: f64| -> f32 {
        let mut w = setup(&TWO_ROOMS);
        let mut sched = door_schedule(false);
        demand(&mut w, TilePos::new(3, 2));
        let mut done = 0.0f64;
        while done < 24.0 {
            step(&mut w, &mut sched, chunk);
            done += chunk;
        }
        door_at(&mut w, 3, 2).progress
    };
    assert!(
        (run(1.0) - run(4.0)).abs() < 1e-5,
        "speed-independent state"
    );
    assert!(run(1.0) >= 1.0, "fully open after one travel time");
}

// =====================================================================================
// Pathfinding
// =====================================================================================

#[test]
fn path_through_auto_door_is_planned_and_walked() {
    let w = setup(&TWO_ROOMS);
    let map = w.resource::<ShipMap>().clone();
    let p = ship_alive::path::find_path(&map, TilePos::new(3, 1), TilePos::new(3, 3), |_| false)
        .expect("closed auto door is still a legal route");
    assert_eq!(p, vec![TilePos::new(3, 2), TilePos::new(3, 3)]);
}

#[test]
fn no_path_through_locked_door() {
    let mut w = setup(&TWO_ROOMS);
    w.resource_mut::<ShipMap>()
        .set_door_state(TilePos::new(3, 2), Default::default());
    let e = door_entity(&mut w);
    w.get_mut::<Door>(e).unwrap().mode = DoorMode::LockClosed;
    // door_system hasn't run; mirror the lock the way the system would.
    w.resource_mut::<ShipMap>().set_door_state(
        TilePos::new(3, 2),
        ship_alive::map::DoorTileState {
            open: 0.0,
            locked: true,
        },
    );
    let map = w.resource::<ShipMap>().clone();
    assert!(
        ship_alive::path::find_path(&map, TilePos::new(3, 1), TilePos::new(3, 3), |_| false)
            .is_none(),
        "locked door is impassable"
    );
    assert!(
        ship_alive::path::find_path(&map, TilePos::new(3, 1), TilePos::new(3, 2), |_| false)
            .is_none()
    );
    // Standing inside the (locked) door tile, pathing OUT stays legal.
    assert!(
        ship_alive::path::find_path(&map, TilePos::new(3, 2), TilePos::new(3, 3), |_| false)
            .is_some(),
        "a crew caught in a locking door can still leave"
    );
}

#[test]
fn no_diagonal_cut_through_door_frame_corner() {
    // Door at (2,1) in a vertical wall (walls north+south, passage E-W).
    let w = setup(&["#####", "#.D.#", "#####"]);
    let map = w.resource::<ShipMap>().clone();
    assert_eq!(door_axis(&map, TilePos::new(2, 1)), Some(DoorAxis::Ew));
    // Approaching the door diagonally from the far side must be forbidden
    // (its side cell is the wall flanking the leaf).
    let p = ship_alive::path::find_path(&map, TilePos::new(1, 1), TilePos::new(3, 1), |_| false);
    let p = p.expect("straight passage through the door exists");
    assert_eq!(p.len(), 2);
    assert_eq!(p[0], TilePos::new(2, 1), "enters the door straight");
}

#[test]
fn door_orientation_inference() {
    let w = setup(&TWO_ROOMS);
    let map = w.resource::<ShipMap>().clone();
    assert_eq!(door_axis(&map, TilePos::new(3, 2)), Some(DoorAxis::Ns));
    // Open hall: no valid orientation.
    let open = setup(&[".....", ".....", "....."]);
    let map2 = open.resource::<ShipMap>().clone();
    assert_eq!(door_axis(&map2, TilePos::new(2, 2)), None);
    // Cross of walls: ambiguous.
    let cross = setup(&["#####", "##.##", "#...#", "##.##", "#####"]);
    let map3 = cross.resource::<ShipMap>().clone();
    assert_eq!(door_axis(&map3, TilePos::new(2, 2)), None);
}

// =====================================================================================
// Thermal integration
// =====================================================================================

#[test]
fn closed_door_blocks_fast_ambient_mixing() {
    // Hot room A, room B at ambient, a door between them. Over a short
    // window an open doorway must raise B far more than the closed seep.
    // (Long windows wash out: the walls' large thermal mass pulls both
    // cases back toward ambient.)
    let run = |open: bool| -> f32 {
        let mut w = setup(&TWO_ROOMS);
        let mut sched = thermal_schedule();
        let mut door_sched = door_schedule(false);
        {
            let mut grid = w.resource_mut::<ThermalGrid>();
            let i_hot = grid.idx(TilePos::new(2, 1));
            grid.amb[i_hot] = 90.0;
            grid.wake(i_hot);
        }
        if open {
            let e = door_entity(&mut w);
            w.get_mut::<Door>(e).unwrap().mode = DoorMode::HoldOpen;
            for _ in 0..30 {
                step(&mut w, &mut door_sched, 1.0);
            }
            assert!(!w
                .resource::<ThermalGrid>()
                .door_sealed_at(TilePos::new(3, 2)));
        }
        // Track the far side's PEAK: the wall mass re-absorbs the pulse
        // after a few dozen steps, so a late sample hides the difference.
        let mut peak = AMBIENT_START;
        for _ in 0..60 {
            step(&mut w, &mut sched, 1.0);
            peak = peak.max(w.resource::<ThermalGrid>().amb_at(TilePos::new(4, 3)));
        }
        peak
    };
    let spread_closed = run(false);
    let spread_open = run(true);
    assert!(
        spread_closed < AMBIENT_START + 1.0,
        "closed door still seeps, but slowly ({spread_closed})"
    );
    assert!(spread_open > AMBIENT_START + 2.0);
}

#[test]
fn open_door_propagates_heat_and_wakes_both_sides() {
    let mut w = setup(&TWO_ROOMS);
    let mut door_sched = door_schedule(false);
    let mut sched = thermal_schedule();
    {
        let mut grid = w.resource_mut::<ThermalGrid>();
        let i_hot = grid.idx(TilePos::new(2, 1));
        grid.amb[i_hot] = 90.0;
        grid.wake(i_hot);
    }
    let e = door_entity(&mut w);
    w.get_mut::<Door>(e).unwrap().mode = DoorMode::HoldOpen;
    for _ in 0..30 {
        step(&mut w, &mut door_sched, 1.0);
    }
    // Unsealing the door woke its tile; conduction now carries heat into
    // room B without any manual wake on the far side.
    assert!(!w
        .resource::<ThermalGrid>()
        .door_sealed_at(TilePos::new(3, 2)));
    let mut peak = AMBIENT_START;
    for _ in 0..60 {
        step(&mut w, &mut sched, 1.0);
        peak = peak.max(w.resource::<ThermalGrid>().amb_at(TilePos::new(3, 3)));
    }
    assert!(
        peak > AMBIENT_START + 2.0,
        "heat crossed the open doorway (peak {peak})"
    );
}

#[test]
fn door_toggle_conserves_heat_exactly() {
    let mut w = setup(&TWO_ROOMS);
    let mut sched = door_schedule(false);
    let mut thermal_sched = thermal_schedule();
    {
        let mut grid = w.resource_mut::<ThermalGrid>();
        let (i_hot, i_cold) = (grid.idx(TilePos::new(2, 1)), grid.idx(TilePos::new(4, 3)));
        grid.amb[i_hot] = 70.0;
        grid.amb[i_cold] = 5.0;
    }
    let devices = ship_alive::thermal::DeviceTiles::default();
    let before = w.resource::<ThermalGrid>().total_heat(&devices);
    // Toggle the door open (and run exchanges), then closed again.
    let e = door_entity(&mut w);
    w.get_mut::<Door>(e).unwrap().mode = DoorMode::HoldOpen;
    for _ in 0..30 {
        step(&mut w, &mut sched, 1.0);
    }
    for _ in 0..200 {
        step(&mut w, &mut thermal_sched, 1.0);
    }
    let mid = w.resource::<ThermalGrid>().total_heat(&devices);
    assert!(
        (mid - before).abs() < 1e-3,
        "no heat created/destroyed by opening (delta {})",
        mid - before
    );
    w.get_mut::<Door>(e).unwrap().mode = DoorMode::LockClosed;
    for _ in 0..40 {
        step(&mut w, &mut sched, 1.0);
    }
    for _ in 0..200 {
        step(&mut w, &mut thermal_sched, 1.0);
    }
    let after = w.resource::<ThermalGrid>().total_heat(&devices);
    assert!(
        (after - before).abs() < 1e-3,
        "toggle never mints heat (delta {})",
        after - before
    );
}

// =====================================================================================
// Cache behavior & performance
// =====================================================================================

#[test]
fn stable_topology_is_not_rebuilt() {
    let mut w = setup(&TWO_ROOMS);
    let mut sched = door_schedule(false);
    let start_rebuilds = w.resource::<Compartments>().rebuilds;
    demand(&mut w, TilePos::new(3, 2));
    for _ in 0..1000 {
        step(&mut w, &mut sched, 1.0);
    }
    let comps = w.resource::<Compartments>();
    assert_eq!(
        comps.rebuilds, start_rebuilds,
        "no geometry change: no structural rebuild"
    );
    // Air recomputes only happened on real seal flips.
    assert!(
        comps.air_recomputes <= 4,
        "recomputes: {}",
        comps.air_recomputes
    );
}

#[test]
fn door_toggles_never_rebuild_structure() {
    let mut w = setup(&TWO_ROOMS);
    let mut sched = door_schedule(false);
    let e = door_entity(&mut w);
    for i in 0..100 {
        w.get_mut::<Door>(e).unwrap().mode = if i % 2 == 0 {
            DoorMode::HoldOpen
        } else {
            DoorMode::LockClosed
        };
        for _ in 0..30 {
            step(&mut w, &mut sched, 1.0);
        }
    }
    let comps = w.resource::<Compartments>();
    assert_eq!(comps.rebuilds, 1, "door state is not geometry");
    assert!(
        comps.air_recomputes > 0,
        "connectivity did update with the seals"
    );
}

/// 128x128 synthetic ship: walls every 4th row/column, a door in every wall
/// segment's middle — many compartments, many doors.
fn big_map() -> ShipMap {
    let n = 128;
    let rows: Vec<String> = (0..n)
        .map(|y| {
            (0..n)
                .map(|x| {
                    if x == 0 || y == 0 || x == n - 1 || y == n - 1 {
                        '#'
                    } else if (y % 4 == 2 && x % 8 == 4 && y > 1 && y < n - 2)
                        || (x % 4 == 2 && y % 8 == 4 && x > 1 && x < n - 2)
                    {
                        'D'
                    } else if y % 4 == 2 || x % 4 == 2 {
                        '#'
                    } else {
                        '.'
                    }
                })
                .collect()
        })
        .collect();
    let refs: Vec<&str> = rows.iter().map(|s| s.as_str()).collect();
    ShipMap::from_layout(&refs).0
}

#[test]
fn perf_128_structural_rebuild_and_stable_steps() {
    let map = big_map();
    let t0 = std::time::Instant::now();
    let comps = Compartments::rebuild(&map);
    let rebuild_us = t0.elapsed().as_micros();
    let door_count = map.iter_tiles().filter(|(_, t)| *t == Tile::Door).count();
    println!(
        "PERF128 rebuild={rebuild_us}us regions={} portals={door_count}",
        comps.regions.len()
    );
    assert!(comps.regions.len() > 200, "many compartments");
    assert!(door_count > 100, "many doors");
    // Generous debug-build bound; the slice only needs "not a frame hog".
    assert!(rebuild_us < 60_000, "rebuild too slow: {rebuild_us}us");

    // No-change step cost: air recompute over the portal graph stays tiny.
    let t1 = std::time::Instant::now();
    for _ in 0..100 {
        let mut c = comps.clone();
        c.recompute_air(&map);
    }
    let air_us = t1.elapsed().as_micros() / 100;
    println!("PERF128 air_recompute_avg={air_us}us");
    assert!(air_us < 2_000, "air recompute too slow: {air_us}us");
}

#[test]
fn perf_128_door_state_step_cost() {
    let (map, spawns) = {
        let n = 64;
        let rows: Vec<String> = (0..n)
            .map(|y| {
                (0..n)
                    .map(|x| {
                        if x == 0 || y == 0 || x == n - 1 || y == n - 1 {
                            '#'
                        } else if (y % 4 == 2 && x % 8 == 4 && y > 1 && y < n - 2)
                            || (x % 4 == 2 && y % 8 == 4 && x > 1 && x < n - 2)
                        {
                            'D'
                        } else if y % 4 == 2 || x % 4 == 2 {
                            '#'
                        } else {
                            '.'
                        }
                    })
                    .collect()
            })
            .collect();
        let refs: Vec<&str> = rows.iter().map(|s| s.as_str()).collect();
        ShipMap::from_layout(&refs)
    };
    let mut w = World::new();
    w.insert_resource(map);
    w.insert_resource(ThermalGrid::new(w.resource::<ShipMap>()));
    w.insert_resource(Compartments::rebuild(w.resource::<ShipMap>()));
    w.insert_resource(DoorDemand::default());
    w.insert_resource(SimClock::default());
    w.insert_resource(ship_alive::log::EventLog::default());
    for req in spawns {
        if let SpawnReq::Door { pos } = req {
            let axis = door_axis(w.resource::<ShipMap>(), pos).unwrap();
            w.spawn((pos, Door::new(axis)));
        }
    }
    let mut sched = door_schedule(false);
    // Half the doors demand passage every step: worst-case toggle churn.
    let doors: Vec<TilePos> = {
        let mut q = w.query::<&TilePos>();
        q.iter(&w).step_by(2).copied().collect()
    };
    let t0 = std::time::Instant::now();
    for _ in 0..200 {
        for p in &doors {
            demand(&mut w, *p);
        }
        step(&mut w, &mut sched, 1.0);
    }
    let per_step_us = t0.elapsed().as_micros() / 200;
    let portals = w.resource::<Compartments>().doors.len();
    println!("PERF64 door_step_avg={per_step_us}us portals={portals}");
    assert!(per_step_us < 3_000, "door step too slow: {per_step_us}us");
    assert_eq!(w.resource::<Compartments>().rebuilds, 1);
}
