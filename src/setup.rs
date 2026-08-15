//! Bootstraps the world from the hand-authored ship layout: map resource,
//! camera, crew, racks and ground items.

use crate::crew::{Crew, CrewTask, Movement};
use crate::items;
use crate::log::EventLog;
use crate::map::{ShipMap, SpawnReq, TilePos, MAP_LAYOUT};
use crate::storage::StorageCell;
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

pub fn setup_world(mut commands: Commands, mut images: ResMut<Assets<Image>>, server: Res<AssetServer>) {
    let (map, spawns) = ShipMap::from_layout(&MAP_LAYOUT);

    // Camera centered on the ship.
    commands.spawn((
        Camera2d,
        Transform::from_xyz(
            map.width as f32 * crate::TILE * 0.5,
            -(map.height as f32 * crate::TILE * 0.5),
            1000.0,
        ),
    ));

    commands.insert_resource(map);
    commands.insert_resource(EventLog::default());
    commands.insert_resource(crate::render::Art::load(&server, &mut images));

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
            SpawnReq::Rack { pos } => {
                commands.spawn((TilePos::new(pos.x, pos.y), StorageCell::default()));
            }
            SpawnReq::Item { pos, kind } => {
                items::spawn_item(&mut commands, pos, kind);
            }
        }
    }
}
