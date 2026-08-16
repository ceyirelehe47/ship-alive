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
    let (mut map, spawns) = ShipMap::from_layout(&MAP_LAYOUT);
    let mut thermal_grid = ThermalGrid::new(&map);

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
            SpawnReq::Door { pos } => {
                let axis =
                    crate::airtight::door_axis(&map, pos).unwrap_or(crate::airtight::DoorAxis::Ns);
                commands.spawn((
                    pos,
                    Footprint::new(pos.x, pos.y, 1, 1),
                    Building {
                        kind: BuildingKind::Door,
                        foot: Footprint::new(pos.x, pos.y, 1, 1),
                        demo_progress: 0.0,
                    },
                    crate::airtight::Door::new(axis),
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
    // Power the starter blower: extend the reactor's cable run north from
    // the existing (15,14) tile through the fabricator footprint and the
    // row-9 wall into the corridor (cables may cross interior walls and run
    // under ducts — independent layers).
    for c in [
        TilePos::new(15, 13),
        TilePos::new(15, 12),
        TilePos::new(15, 11),
        TilePos::new(15, 10),
        TilePos::new(15, 9),
        TilePos::new(15, 8),
        TilePos::new(15, 7),
        TilePos::new(14, 7),
        TilePos::new(13, 7),
        TilePos::new(12, 7),
    ] {
        cables.set(c, true);
    }
    commands.insert_resource(cables);
    commands.insert_resource(pipes);
    commands.insert_resource(water);
    commands.insert_resource(crate::coolant::CoolantState::default());
    commands.insert_resource(crate::coolant::CoolantStats::default());
    commands.insert_resource(crate::thermal::DeviceTiles::sized(
        (map.width * map.height) as usize,
    ));
    commands.insert_resource(crate::thermal::ThermalStats::default());
    // Standard atmosphere boot fill: every gas cell (floor / machine / door)
    // at ~101.3 kPa, 21 kPa O₂ partial, low CO₂, no pollutant. The gas heat
    // capacities replace the thermal grid's boot-time constants immediately.
    let mut atmo = crate::atmosphere::AtmosphereGrid::new(&map);
    atmo.sync_all_gas_caps(&mut thermal_grid);
    // Starter ventilation network (Slice 6): FABRICATION ↔ corridor ↔
    // CREW QUARTERS. 19 duct tiles (passing under interior walls exactly as
    // a player would lay them), two vents, a standby eastbound blower and a
    // prefilled tank just west of it (so tank → blower → vent B supplies
    // CREW, and vent A → tank charges with the blower reversed). Ducts boot
    // empty and fill from the rooms (~190 mol, a <2% pressure dip); the
    // tank's standard fill means the balanced system moves nothing at boot.
    let mut ducts = crate::ventilation::DuctGrid::new(width, height);
    let mut duct_tiles: Vec<TilePos> = Vec::new();
    for y in 7..=13 {
        duct_tiles.push(TilePos::new(10, y));
    }
    for x in 11..=18 {
        duct_tiles.push(TilePos::new(x, 7));
    }
    for y in 3..=6 {
        duct_tiles.push(TilePos::new(18, y));
    }
    for t in duct_tiles {
        ducts.set(t, true);
    }
    let mut vent = |pos: TilePos| {
        commands.spawn((
            pos,
            Footprint::new(pos.x, pos.y, 1, 1),
            Building {
                kind: BuildingKind::Vent,
                foot: Footprint::new(pos.x, pos.y, 1, 1),
                demo_progress: 0.0,
            },
            crate::ventilation::Vent::default(),
        ));
    };
    vent(TilePos::new(10, 13)); // FABRICATION
    vent(TilePos::new(18, 3)); // CREW QUARTERS
    let blower_pos = TilePos::new(12, 7);
    commands.spawn((
        blower_pos,
        Footprint::new(blower_pos.x, blower_pos.y, 1, 1),
        Building {
            kind: BuildingKind::Blower,
            foot: Footprint::new(blower_pos.x, blower_pos.y, 1, 1),
            demo_progress: 0.0,
        },
        // Boots in standby: an enabled blower would slowly pump FAB air
        // into CREW at otherwise-uniform pressure (§77 — the starter
        // system must not disturb the environment). The player runs it.
        crate::ventilation::Blower {
            dir: crate::ventilation::Dir4::East,
            enabled: false,
            last_flow: 0.0,
        },
        PowerRole::consumer(crate::ventilation::BLOWER_DEMAND),
        PowerStatus::default(),
        ThermalBody::passive(4.0),
        ThermalState::default(),
    ));
    let tank_pos = TilePos::new(11, 7);
    map.set_tile(tank_pos, crate::map::Tile::Machine);
    thermal_grid.tile_changed(tank_pos, crate::map::Tile::Machine);
    atmo.tile_changed(&map, tank_pos, crate::map::Tile::Machine);
    commands.spawn((
        tank_pos,
        Footprint::new(tank_pos.x, tank_pos.y, 1, 1),
        Building {
            kind: BuildingKind::GasTank,
            foot: Footprint::new(tank_pos.x, tank_pos.y, 1, 1),
            demo_progress: 0.0,
        },
        crate::ventilation::GasTank::prefilled_standard(),
        ThermalBody::passive(12.0),
        ThermalState::default(),
    ));
    commands.insert_resource(thermal_grid);
    commands.insert_resource(ducts);
    commands.insert_resource(crate::ventilation::DuctTopology::default());
    commands.insert_resource(crate::ventilation::VentStats::default());
    commands.insert_resource(crate::ventilation::VentSummary::default());
    commands.insert_resource(crate::atmosphere::AtmoStats::from_grid(&atmo));
    commands.insert_resource(crate::atmosphere::AtmoSummary::default());
    commands.insert_resource(atmo);
    commands.insert_resource(crate::airtight::Compartments::rebuild(&map));
    commands.insert_resource(map);
}
