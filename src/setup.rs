//! Bootstraps the world from the hand-authored ship layout: map resource,
//! camera, crew, racks, fabricator, reactor, underfloor cables/pipes, the
//! pre-installed coolant loop and ground items.
//!
//! Map-spawned racks, the starter fabricator and the starter reactor carry
//! the same components the construction system produces, so everything the
//! player can see blocking a tile can also be torn down and moved.

use crate::building::{Building, BuildingKind, Footprint};
use crate::coolant::{PipeGrid, WaterGrid};
use crate::crew::{Crew, CrewTask, Movement};
use crate::items;
use crate::log::EventLog;
use crate::map::{ShipMap, SpawnReq, TilePos, MAP_LAYOUT};
use crate::power::{CableGrid, PowerRole, PowerStatus, FABRICATOR_DEMAND};
use crate::storage::StorageCell;
use crate::thermal::{ThermalBody, ThermalGrid, ThermalState};
use bevy::prelude::*;

/// Names and suit colors of the four starter crew members.
const CREW_ROSTER: [(&str, [f32; 3]); 4] = [
    ("Ava", [0.98, 0.45, 0.42]),
    ("Rex", [0.45, 0.65, 0.98]),
    ("Mio", [0.50, 0.92, 0.55]),
    ("Zed", [0.80, 0.55, 0.95]),
];

pub struct SetupPlugin;

impl Plugin for SetupPlugin {
    fn build(&self, _app: &mut App) {
        // `setup_world` is registered once in main.rs with explicit ordering
        // before the render startup systems; the plugin stays as a marker.
    }
}

pub fn setup_world(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    server: Res<AssetServer>,
) {
    let (map, spawns) = ShipMap::from_layout(&MAP_LAYOUT);
    let thermal_grid = ThermalGrid::new(&map);

    // Camera centered on the ship.
    commands.spawn((
        Camera2d,
        Transform::from_xyz(
            map.width as f32 * crate::TILE * 0.5,
            -(map.height as f32 * crate::TILE * 0.5),
            1000.0,
        ),
    ));

    let width = map.width;
    let height = map.height;
    commands.insert_resource(EventLog::default());
    commands.insert_resource(crate::stats::Stats::default());
    commands.insert_resource(crate::render::Art::load(&server, &mut images));
    let mut cables = CableGrid::new(width, height);
    let mut pipes = PipeGrid::new(width, height);
    let mut water = WaterGrid::new(width, height);
    // Reservoir tiles hold extra water; collected while spawning so the
    // pre-fill below can top them up too.
    let mut reservoir_tiles: Vec<TilePos> = Vec::new();

    let mut crew_index = 0;
    for req in spawns {
        match req {
            SpawnReq::Crew { pos } => {
                let (name, tint) = CREW_ROSTER[crew_index.min(CREW_ROSTER.len() - 1)];
                crew_index += 1;
                let mut crew = Crew::new(name, Color::srgb(tint[0], tint[1], tint[2]));
                // Stagger job scans so the crew do not claim in lock-step.
                crew.next_scan = 0.05 * crew_index as f64;
                commands.spawn((
                    TilePos::new(pos.x, pos.y),
                    crew,
                    CrewTask::default(),
                    Movement::default(),
                ));
            }
            SpawnReq::Rack { pos, fill } => {
                let cell = match fill {
                    Some((kind, n)) => StorageCell::with_stock(kind, n),
                    None => StorageCell::default(),
                };
                commands.spawn((
                    TilePos::new(pos.x, pos.y),
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
                commands.spawn((
                    TilePos::new(pos.x, pos.y),
                    crate::production::Fabricator::default(),
                    Footprint::new(pos.x, pos.y, 2, 2),
                    Building {
                        kind: BuildingKind::Fabricator,
                        foot: Footprint::new(pos.x, pos.y, 2, 2),
                        demo_progress: 0.0,
                    },
                    PowerRole::consumer(FABRICATOR_DEMAND),
                    PowerStatus::default(),
                    ThermalBody::fabricator(),
                    ThermalState::default(),
                ));
            }
            SpawnReq::Reactor { pos } => {
                commands.spawn((
                    TilePos::new(pos.x, pos.y),
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
                commands.spawn((
                    pos,
                    Footprint::new(pos.x, pos.y, 1, 1),
                    Building {
                        kind: BuildingKind::Pump,
                        foot: Footprint::new(pos.x, pos.y, 1, 1),
                        demo_progress: 0.0,
                    },
                    crate::coolant::Pump,
                    PowerRole::consumer(crate::coolant::PUMP_DEMAND),
                    PowerStatus::default(),
                    ThermalBody::pump(),
                    ThermalState::default(),
                ));
            }
            SpawnReq::Reservoir { pos } => {
                reservoir_tiles.push(pos);
                commands.spawn((
                    pos,
                    Footprint::new(pos.x, pos.y, 1, 1),
                    Building {
                        kind: BuildingKind::Reservoir,
                        foot: Footprint::new(pos.x, pos.y, 1, 1),
                        demo_progress: 0.0,
                    },
                    crate::coolant::Reservoir,
                    ThermalBody::passive(20.0),
                    ThermalState::default(),
                ));
            }
            SpawnReq::HeatExchanger { pos } => {
                commands.spawn((
                    pos,
                    Footprint::new(pos.x, pos.y, 1, 1),
                    Building {
                        kind: BuildingKind::HeatExchanger,
                        foot: Footprint::new(pos.x, pos.y, 1, 1),
                        demo_progress: 0.0,
                    },
                    crate::coolant::HeatExchanger,
                    ThermalBody::passive(8.0),
                    ThermalState::default(),
                ));
            }
            SpawnReq::Radiator { pos } => {
                let hull_ok = crate::building::hull_adjacent(&map, pos);
                commands.spawn((
                    pos,
                    Footprint::new(pos.x, pos.y, 1, 1),
                    Building {
                        kind: BuildingKind::Radiator,
                        foot: Footprint::new(pos.x, pos.y, 1, 1),
                        demo_progress: 0.0,
                    },
                    crate::coolant::Radiator { hull_ok },
                    ThermalBody::passive(10.0),
                    ThermalState::default(),
                ));
            }
            SpawnReq::Item { pos, kind } => {
                items::spawn_item(&mut commands, pos, kind);
            }
        }
    }
    // The starter loop ships filled with headroom (80%): circulation works
    // the same, and tearing a pipe down can push its water into the
    // neighbours instead of spilling.
    const STARTER_FILL: f32 = 5.0;
    const STARTER_RESERVOIR_FILL: f32 = 50.0;
    for pos in pipes.iter_pipes() {
        water.fill(pos, STARTER_FILL, crate::thermal::AMBIENT_START);
    }
    for pos in &reservoir_tiles {
        water.fill(*pos, STARTER_RESERVOIR_FILL, crate::thermal::AMBIENT_START);
    }
    commands.insert_resource(cables);
    commands.insert_resource(pipes);
    commands.insert_resource(water);
    commands.insert_resource(thermal_grid);
    commands.insert_resource(crate::coolant::CoolantState::default());
    commands.insert_resource(crate::coolant::CoolantStats::default());
    commands.insert_resource(crate::thermal::DeviceTiles::default());
    commands.insert_resource(crate::thermal::ThermalStats::default());
    commands.insert_resource(map);
}
