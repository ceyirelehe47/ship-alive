//! Rendering: every gameplay entity (crew, item, rack, building, blueprint)
//! is represented by a separate "visual" entity linked through
//! `Visual { target }`. Logic code never touches sprites, and dead targets
//! are cleaned up automatically, so despawning an entity can never leak or
//! orphan its visuals.
//!
//! Art is loaded from `assets/art/*.png` when present; missing files fall
//! back to procedurally generated colored quads so the game stays playable
//! before the art pass (see `Art::load`).

use crate::building::{self, Blueprint, Building, BuildingKind, Footprint, MarkedForDeconstruct};
use crate::crew::{Crew, CrewTask, HaulDest, HaulPhase, Movement};
use crate::input::{BuildMode, Selected, Selection, Tool};
use crate::items::{CarriedBy, Item, ItemKind, MarkedForHaul, NoPathUntil, ReservedBy};
use crate::map::{ShipMap, TilePos};
use crate::power::{CableGrid, PowerOverlay, PowerRole, PowerState, PowerStatus};
use crate::production::{MachineState, RECIPE};
use crate::storage::StorageCell;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// Which sprite role a visual entity plays.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Rack,
    RackLabel,
    ItemSprite,
    ItemRing,
    CrewSprite,
    CrewLabel,
    CrewCarry,
    /// Finished building (wall/door/fabricator sprite, sized to footprint).
    BuildingSprite,
    /// Construction blueprint ghost sprite.
    BlueprintSprite,
    /// Blueprint materials/progress text.
    BlueprintLabel,
    /// Fabricator state ring.
    FabRing,
    /// Fabricator state text.
    FabLabel,
}

#[derive(Component)]
pub struct Visual {
    pub target: Entity,
    pub role: Role,
}

/// Marker on logic entities that already have their visuals spawned.
#[derive(Component)]
pub struct HasVisual;

/// Tags the power overlay root entity.
#[derive(Component)]
pub struct PowerOverlayRoot;

/// Power overlay rendering: one root entity whose children (cable tiles +
/// device rings) are rebuilt whenever the grid or statuses change.
#[derive(Resource)]
pub struct PowerOverlayVis {
    pub root: Entity,
    pub last_sig: u64,
}

/// Persistent selection/path marker entities (pooled, hidden when unused).
#[derive(Resource)]
pub struct Markers {
    pub selection: Entity,
    pub hover: Entity,
    pub target: Entity,
    pub dots: Vec<Entity>,
    /// Build-tool placement ghost.
    pub ghost: Entity,
    pub ghost_label: Entity,
}

/// All sprite/texture handles used by the game.
#[derive(Resource)]
pub struct Art {
    pub floor: Handle<Image>,
    pub wall: Handle<Image>,
    pub wall_built: Handle<Image>,
    pub door: Handle<Image>,
    pub rack: Handle<Image>,
    pub fabricator: Handle<Image>,
    pub reactor: Handle<Image>,
    pub crate_: Handle<Image>,
    pub ore: Handle<Image>,
    pub part: Handle<Image>,
    pub crew: Handle<Image>,
    pub ring: Handle<Image>,
    pub dot: Handle<Image>,
}

impl Art {
    pub fn item(&self, kind: ItemKind) -> &Handle<Image> {
        match kind {
            ItemKind::Crate => &self.crate_,
            ItemKind::Ore => &self.ore,
            ItemKind::Part => &self.part,
        }
    }

    pub fn building(&self, kind: BuildingKind) -> &Handle<Image> {
        match kind {
            BuildingKind::Wall => &self.wall_built,
            BuildingKind::Door => &self.door,
            BuildingKind::Rack => &self.rack,
            BuildingKind::Fabricator => &self.fabricator,
            BuildingKind::Reactor => &self.reactor,
            BuildingKind::PowerCable => &self.dot,
        }
    }

    pub fn load(server: &AssetServer, images: &mut Assets<Image>) -> Self {
        let mut fill = |path: &str, color: [u8; 4]| -> Handle<Image> {
            let file = format!("assets/{path}");
            if std::path::Path::new(&file).exists() {
                server.load(path)
            } else {
                let img = bevy::image::Image::new_fill(
                    Extent3d {
                        width: 32,
                        height: 32,
                        depth_or_array_layers: 1,
                    },
                    TextureDimension::D2,
                    &color,
                    TextureFormat::Rgba8UnormSrgb,
                    bevy::asset::RenderAssetUsages::MAIN_WORLD
                        | bevy::asset::RenderAssetUsages::RENDER_WORLD,
                );
                images.add(img)
            }
        };
        Self {
            floor: fill("art/floor.png", [64, 70, 86, 255]),
            wall: fill("art/wall.png", [34, 38, 50, 255]),
            wall_built: fill("art/wall_built.png", [72, 82, 104, 255]),
            door: fill("art/door.png", [96, 148, 178, 255]),
            rack: fill("art/rack.png", [58, 118, 118, 255]),
            fabricator: fill("art/fabricator.png", [112, 122, 146, 255]),
            reactor: fill("art/reactor.png", [92, 168, 110, 255]),
            crate_: fill("art/crate.png", [198, 166, 112, 255]),
            ore: fill("art/ore.png", [150, 92, 62, 255]),
            part: fill("art/part.png", [134, 134, 172, 255]),
            crew: fill("art/crew.png", [235, 235, 235, 255]),
            ring: fill("art/ring.png", [255, 178, 42, 110]),
            dot: fill("art/dot.png", [90, 220, 255, 200]),
        }
    }
}

fn sprite(image: Handle<Image>, size: f32, z: f32, pos: Vec2, color: Color) -> (Sprite, Transform) {
    (
        Sprite {
            image,
            custom_size: Some(Vec2::splat(size)),
            color,
            ..default()
        },
        Transform::from_translation(pos.extend(z)),
    )
}

/// World-space center of a footprint rect.
fn foot_world_pos(foot: &building::Footprint) -> Vec2 {
    Vec2::new(
        (foot.x as f32 + foot.w as f32 / 2.0) * crate::TILE,
        -(foot.y as f32 + foot.h as f32 / 2.0) * crate::TILE,
    )
}

pub struct RenderPlugin;

impl Plugin for RenderPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Startup,
            (spawn_tile_visuals, spawn_room_labels, spawn_markers),
        );
        app.add_systems(
            Update,
            (
                ensure_visuals_system,
                sync_crew_visuals_system,
                sync_item_visuals_system,
                sync_rack_labels_system,
                sync_building_visuals_system,
                sync_selection_system,
                ghost_system,
                power_overlay_system,
                cleanup_visuals_system,
            )
                .chain()
                .in_set(crate::Set::Sync),
        );
    }
}

/// Static floor/wall tiles — spawned once from the grid.
pub fn spawn_tile_visuals(mut commands: Commands, map: Res<ShipMap>, art: Res<Art>) {
    for (pos, tile) in map.iter_tiles() {
        let img = match tile {
            crate::map::Tile::Wall => art.wall.clone(),
            _ => art.floor.clone(),
        };
        let z = match tile {
            crate::map::Tile::Wall => 0.05,
            _ => 0.0,
        };
        commands.spawn(sprite(
            img,
            crate::TILE,
            z,
            map.world_pos(pos),
            Color::WHITE,
        ));
    }
}

/// Room name labels and a faint tint over the storage bay, so the ship layout
/// reads at a glance without any tutorial.
fn spawn_room_labels(mut commands: Commands) {
    let rooms: [(&str, i32, i32, i32, i32); 6] = [
        ("CARGO HOLD", 1, 1, 10, 5),
        ("CREW QUARTERS", 12, 1, 21, 5),
        ("ORE BAY", 23, 1, 34, 5),
        ("PARTS ROOM", 1, 10, 10, 17),
        ("FABRICATION", 12, 10, 21, 17),
        ("STORAGE", 23, 10, 34, 17),
    ];
    for (name, x0, y0, x1, y1) in rooms {
        // Tile-rect → world rect.
        let left = x0 as f32 * crate::TILE;
        let right = (x1 + 1) as f32 * crate::TILE;
        let top = -(y0 as f32) * crate::TILE;
        let bottom = -((y1 + 1) as f32) * crate::TILE;
        let center = Vec2::new((left + right) * 0.5, (top + bottom) * 0.5);
        let size = Vec2::new(right - left, top - bottom);

        if name == "STORAGE" {
            commands.spawn((
                Sprite::from_color(Color::srgba(1.0, 0.72, 0.25, 0.06), size),
                Transform::from_translation(center.extend(0.01)),
            ));
        }
        commands.spawn((
            Text2d::new(name),
            TextFont {
                font_size: 13.0,
                ..default()
            },
            TextColor(Color::srgba(0.62, 0.68, 0.78, 0.5)),
            Transform::from_translation(
                (center + Vec2::new(0.0, size.y * 0.5 - 14.0)).extend(0.02),
            ),
        ));
    }
}

/// Marker pool for the current selection.
pub fn spawn_markers(mut commands: Commands, art: Res<Art>) {
    let selection = commands
        .spawn((
            Sprite {
                image: art.ring.clone(),
                custom_size: Some(Vec2::splat(crate::TILE * 1.1)),
                color: Color::srgb(1.0, 0.85, 0.2),
                ..default()
            },
            Transform::from_translation(Vec3::Z * 0.9),
            Visibility::Hidden,
        ))
        .id();
    let hover = commands
        .spawn((
            Sprite {
                image: art.ring.clone(),
                custom_size: Some(Vec2::splat(crate::TILE * 1.05)),
                color: Color::srgba(0.9, 0.95, 1.0, 0.7),
                ..default()
            },
            Transform::from_translation(Vec3::Z * 0.85),
            Visibility::Hidden,
        ))
        .id();
    let target = commands
        .spawn((
            Sprite {
                image: art.dot.clone(),
                custom_size: Some(Vec2::splat(crate::TILE * 0.9)),
                color: Color::srgb(0.4, 1.0, 0.5),
                ..default()
            },
            Transform::from_translation(Vec3::Z * 0.42),
            Visibility::Hidden,
        ))
        .id();
    let dots = (0..64)
        .map(|_| {
            commands
                .spawn((
                    Sprite {
                        image: art.dot.clone(),
                        custom_size: Some(Vec2::splat(crate::TILE * 0.16)),
                        color: Color::srgb(0.35, 0.85, 1.0),
                        ..default()
                    },
                    Transform::from_translation(Vec3::Z * 0.15),
                    Visibility::Hidden,
                ))
                .id()
        })
        .collect();
    let ghost = commands
        .spawn((
            Sprite {
                image: art.ring.clone(),
                custom_size: Some(Vec2::splat(crate::TILE * 0.95)),
                color: Color::srgba(0.4, 1.0, 0.5, 0.8),
                ..default()
            },
            Transform::from_translation(Vec3::Z * 0.8),
            Visibility::Hidden,
        ))
        .id();
    let ghost_label = commands
        .spawn((
            Text2d::new(""),
            TextFont {
                font_size: 12.0,
                ..default()
            },
            TextColor(Color::WHITE),
            Transform::from_translation(Vec3::new(0.0, 0.0, 0.82)),
            Visibility::Hidden,
        ))
        .id();
    let power_root = commands
        .spawn((PowerOverlayRoot, Transform::default(), Visibility::Hidden))
        .id();
    commands.insert_resource(Markers {
        selection,
        hover,
        target,
        dots,
        ghost,
        ghost_label,
    });
    commands.insert_resource(PowerOverlayVis {
        root: power_root,
        last_sig: 0,
    });
}

/// Spawn visuals for logic entities that do not have them yet.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn ensure_visuals_system(
    mut commands: Commands,
    map: Res<ShipMap>,
    art: Res<Art>,
    crews: Query<(Entity, &TilePos, &Crew), Without<HasVisual>>,
    items: Query<(Entity, &TilePos, &Item), Without<HasVisual>>,
    racks: Query<(Entity, &TilePos), (With<StorageCell>, Without<HasVisual>)>,
    blueprints: Query<(Entity, &building::Footprint, &Blueprint), Without<HasVisual>>,
    buildings: Query<
        (Entity, &building::Footprint, &Building),
        (Without<HasVisual>, Without<StorageCell>),
    >,
) {
    for (e, pos, item) in items.iter() {
        let p = map.world_pos(*pos);
        commands.spawn((
            Visual {
                target: e,
                role: Role::ItemSprite,
            },
            sprite(
                art.item(item.kind).clone(),
                crate::TILE * 0.62,
                0.35,
                p,
                Color::WHITE,
            ),
        ));
        commands.spawn((
            Visual {
                target: e,
                role: Role::ItemRing,
            },
            sprite(art.ring.clone(), crate::TILE * 0.95, 0.45, p, Color::WHITE),
            Visibility::Hidden,
        ));
        commands.entity(e).insert(HasVisual);
    }
    for (e, pos, crew) in crews.iter() {
        let p = map.world_pos(*pos);
        commands.spawn((
            Visual {
                target: e,
                role: Role::CrewSprite,
            },
            sprite(art.crew.clone(), crate::TILE * 0.8, 0.6, p, crew.tint),
        ));
        commands.spawn((
            Visual {
                target: e,
                role: Role::CrewLabel,
            },
            Text2d::new(crew.name.clone()),
            TextFont {
                font_size: 11.0,
                ..default()
            },
            TextColor(crew.tint),
            Transform::from_translation((p + Vec2::new(0.0, -22.0)).extend(0.8)),
        ));
        commands.spawn((
            Visual {
                target: e,
                role: Role::CrewCarry,
            },
            sprite(
                art.crate_.clone(),
                crate::TILE * 0.34,
                0.7,
                p + Vec2::new(0.0, 24.0),
                Color::WHITE,
            ),
            Visibility::Hidden,
        ));
        commands.entity(e).insert(HasVisual);
    }
    for (e, pos) in racks.iter() {
        let p = map.world_pos(*pos);
        commands.spawn((
            Visual {
                target: e,
                role: Role::Rack,
            },
            sprite(art.rack.clone(), crate::TILE * 0.95, 0.2, p, Color::WHITE),
        ));
        commands.spawn((
            Visual {
                target: e,
                role: Role::RackLabel,
            },
            Text2d::new("0/4"),
            TextFont {
                font_size: 13.0,
                ..default()
            },
            TextColor(Color::srgb(1.0, 0.93, 0.45)),
            Transform::from_translation((p + Vec2::new(0.0, 12.0)).extend(0.3)),
        ));
        commands.entity(e).insert(HasVisual);
    }
    for (e, foot, bp) in blueprints.iter() {
        let p = foot_world_pos(foot);
        let d = building::def(bp.kind);
        let size = crate::TILE * d.w as f32 * 0.95;
        commands.spawn((
            Visual {
                target: e,
                role: Role::BlueprintSprite,
            },
            sprite(
                art.building(bp.kind).clone(),
                size,
                0.25,
                p,
                Color::srgba(0.55, 0.85, 1.0, 0.45),
            ),
        ));
        commands.spawn((
            Visual {
                target: e,
                role: Role::BlueprintLabel,
            },
            Text2d::new(""),
            TextFont {
                font_size: 12.0,
                ..default()
            },
            TextColor(Color::srgb(0.65, 0.9, 1.0)),
            Transform::from_translation((p + Vec2::new(0.0, 14.0)).extend(0.5)),
        ));
        commands.entity(e).insert(HasVisual);
    }
    for (e, foot, b) in buildings.iter() {
        let p = foot_world_pos(foot);
        let d = building::def(b.kind);
        let size = crate::TILE * d.w as f32 * 0.98;
        commands.spawn((
            Visual {
                target: e,
                role: Role::BuildingSprite,
            },
            sprite(art.building(b.kind).clone(), size, 0.15, p, Color::WHITE),
        ));
        if b.kind == BuildingKind::Fabricator || b.kind == BuildingKind::Reactor {
            commands.spawn((
                Visual {
                    target: e,
                    role: Role::FabRing,
                },
                sprite(
                    art.ring.clone(),
                    crate::TILE * 2.0 * 0.98,
                    0.4,
                    p,
                    Color::WHITE,
                ),
            ));
            commands.spawn((
                Visual {
                    target: e,
                    role: Role::FabLabel,
                },
                Text2d::new(""),
                TextFont {
                    font_size: 12.0,
                    ..default()
                },
                TextColor(Color::WHITE),
                Transform::from_translation((p + Vec2::new(0.0, 40.0)).extend(0.5)),
            ));
        }
        commands.entity(e).insert(HasVisual);
    }
}

/// Interpolated crew position (tile center → next tile center by progress).
fn crew_world_pos(map: &ShipMap, pos: &TilePos, mov: &Movement) -> Vec2 {
    let from = map.world_pos(*pos);
    if mov.path.is_empty() {
        from
    } else {
        // `progress` is a distance budget toward the next tile; normalize it
        // by the current step's length (diagonal steps are √2 long) so the
        // interpolation factor stays 0..1 in every direction.
        let need = crate::path::step_length(*pos, mov.path[0]);
        let t = (mov.progress / need).clamp(0.0, 1.0);
        from.lerp(map.world_pos(mov.path[0]), t)
    }
}

/// Crew sprites interpolate between tiles; labels and carry icons follow.
/// Idle crew are dimmed so working vs. standing-around reads at a glance.
#[allow(clippy::type_complexity)]
fn sync_crew_visuals_system(
    map: Res<ShipMap>,
    art: Res<Art>,
    crews: Query<(Entity, &Crew, &CrewTask, &TilePos, &Movement)>,
    items: Query<(&CarriedBy, &Item)>,
    mut sprites: Query<(&Visual, &mut Transform, &mut Sprite, &mut Visibility), Without<Text2d>>,
    mut labels: Query<(&Visual, &mut Transform), With<Text2d>>,
) {
    for (e, crew, task, pos, mov) in crews.iter() {
        let p = crew_world_pos(&map, pos, mov);
        let idle = matches!(task, CrewTask::Idle(_));
        for (v, mut tf) in labels.iter_mut() {
            if v.target != e {
                continue;
            }
            if v.role == Role::CrewLabel {
                tf.translation = (p + Vec2::new(0.0, -22.0)).extend(0.8);
            }
        }
        for (v, mut tf, mut sprite, mut vis) in sprites.iter_mut() {
            if v.target != e {
                continue;
            }
            match v.role {
                Role::CrewSprite => {
                    tf.translation = p.extend(0.6);
                    sprite.color = if idle { dimmed(crew.tint) } else { crew.tint };
                }
                Role::CrewCarry => {
                    let carried = items.iter().find(|(c, _)| c.0 == e).map(|(_, i)| i.kind);
                    if let Some(kind) = carried {
                        tf.translation = (p + Vec2::new(0.0, 24.0)).extend(0.7);
                        sprite.image = art.item(kind).clone();
                        *vis = Visibility::Visible;
                    } else {
                        *vis = Visibility::Hidden;
                    }
                }
                _ => {}
            }
        }
    }
}

/// Idle crews render at reduced brightness.
fn dimmed(c: Color) -> Color {
    let s = Srgba::from(c);
    Color::srgba(s.red * 0.45, s.green * 0.45, s.blue * 0.45, 1.0)
}

/// Item sprites sit on their tile; rings show marked state (tinted with the
/// claimer's color once a crew member has claimed the item, red while the
/// claim system considers it unreachable); carried items disappear from the
/// ground and ride along above their carrier.
#[allow(clippy::type_complexity)]
fn sync_item_visuals_system(
    map: Res<ShipMap>,
    time: Res<Time<Virtual>>,
    items: Query<
        (
            Entity,
            &TilePos,
            Option<&MarkedForHaul>,
            Option<&ReservedBy>,
            Option<&CarriedBy>,
            Option<&NoPathUntil>,
        ),
        With<Item>,
    >,
    crews: Query<(Entity, &Crew)>,
    mut sprites: Query<(&Visual, &mut Transform, &mut Sprite, &mut Visibility), Without<Text2d>>,
) {
    let now = time.elapsed().as_secs_f64();
    // Stack offset so several items on one tile remain visible.
    let mut per_tile: std::collections::HashMap<TilePos, usize> = std::collections::HashMap::new();
    for (_, pos, _, _, carried, _) in items.iter() {
        if carried.is_none() {
            *per_tile.entry(*pos).or_insert(0) += 1;
        }
    }
    let mut seen: std::collections::HashMap<TilePos, usize> = std::collections::HashMap::new();

    for (e, pos, marked, reserved, carried, cooled) in items.iter() {
        let carried_now = carried.is_some();
        let mut p = map.world_pos(*pos);
        if !carried_now && *per_tile.get(pos).unwrap_or(&1) > 1 {
            let idx = seen.entry(*pos).or_insert(0);
            p.x += (*idx as f32 - 1.5) * 6.0;
            *idx += 1;
        }
        let ring_color = if cooled.is_some_and(|c| c.0 > now) {
            Color::srgb(1.0, 0.3, 0.25)
        } else if let Some(claimer_tint) =
            reserved.and_then(|r| crews.iter().find(|(ce, _)| *ce == r.0).map(|(_, c)| c.tint))
        {
            claimer_tint
        } else {
            Color::WHITE
        };
        for (v, mut tf, mut sprite, mut vis) in sprites.iter_mut() {
            if v.target != e {
                continue;
            }
            match v.role {
                Role::ItemSprite => {
                    *vis = if carried_now {
                        Visibility::Hidden
                    } else {
                        Visibility::Visible
                    };
                    tf.translation = p.extend(0.35);
                }
                Role::ItemRing => {
                    *vis = if marked.is_some() && !carried_now {
                        Visibility::Visible
                    } else {
                        Visibility::Hidden
                    };
                    sprite.color = ring_color;
                    tf.translation = p.extend(0.45);
                }
                _ => {}
            }
        }
    }
}

/// Rack count labels.
fn sync_rack_labels_system(
    racks: Query<(Entity, &StorageCell), Changed<StorageCell>>,
    mut labels: Query<(&Visual, &mut Text2d)>,
) {
    for (e, cell) in racks.iter() {
        for (v, mut text) in labels.iter_mut() {
            if v.target == e && v.role == Role::RackLabel {
                text.0 = format!("{} {}", cell.label(), cell.filter_label());
            }
        }
    }
}

/// Building & blueprint visuals: deconstruct tint + progress, blueprint
/// materials/progress text, fabricator state ring and label.
#[allow(clippy::type_complexity)]
fn sync_building_visuals_system(
    blueprints: Query<(Entity, &Blueprint), Changed<Blueprint>>,
    buildings: Query<(Entity, &Building, Option<&MarkedForDeconstruct>), Changed<Building>>,
    fabs: Query<(
        Entity,
        &crate::production::Fabricator,
        &crate::power::PowerStatus,
    )>,
    generators: Query<(Entity, &crate::power::PowerRole, &crate::power::PowerStatus)>,
    mut sprites: Query<(&Visual, &mut Sprite), Without<Text2d>>,
    mut labels: Query<(&Visual, &mut Text2d, &mut TextColor)>,
) {
    for (e, bp) in blueprints.iter() {
        let label = if bp.progress > 0.0 {
            format!("{}%", (bp.progress * 100.0) as u32)
        } else {
            bp.materials_label()
        };
        for (v, mut text, _) in labels.iter_mut() {
            if v.target == e && v.role == Role::BlueprintLabel {
                text.0 = label.clone();
            }
        }
    }
    for (e, _, marked) in buildings.iter() {
        for (v, mut sprite) in sprites.iter_mut() {
            if v.target == e && v.role == Role::BuildingSprite {
                sprite.color = if marked.is_some() {
                    Color::srgb(1.0, 0.8, 0.25)
                } else {
                    Color::WHITE
                };
            }
        }
    }
    // Fabricators: machine state, with power problems overriding the look so
    // an unpowered machine reads as such in the normal view too.
    for (e, f, power) in fabs.iter() {
        let state = f.state();
        let (ring, text, text_color) = if !power.ok() {
            (
                crate::power::PowerStatus::color(*power),
                format!("NO POWER — {}", power.label()),
                Color::srgb(1.0, 0.55, 0.45),
            )
        } else {
            match state {
                MachineState::NoOrder => (
                    Color::srgba(0.6, 0.65, 0.7, 0.35),
                    "no order".to_string(),
                    Color::WHITE,
                ),
                MachineState::WaitingInput => (
                    Color::srgba(1.0, 0.75, 0.25, 0.6),
                    format!("need {} ore", RECIPE.in_qty),
                    Color::WHITE,
                ),
                MachineState::WaitingWorker => (
                    Color::srgba(0.35, 0.75, 1.0, 0.6),
                    "waiting for worker".to_string(),
                    Color::WHITE,
                ),
                MachineState::Working => (
                    Color::srgba(0.35, 1.0, 0.5, 0.7),
                    format!("working {}%", (f.progress * 100.0) as u32),
                    Color::srgb(0.55, 1.0, 0.65),
                ),
                MachineState::OutputBlocked => (
                    Color::srgba(1.0, 0.35, 0.3, 0.7),
                    "output blocked".to_string(),
                    Color::srgb(1.0, 0.5, 0.4),
                ),
            }
        };
        for (v, mut sprite) in sprites.iter_mut() {
            if v.target == e && v.role == Role::FabRing {
                sprite.color = ring;
            }
        }
        for (v, mut t, mut c) in labels.iter_mut() {
            if v.target == e && v.role == Role::FabLabel {
                t.0 = text.clone();
                c.0 = text_color;
            }
        }
    }
    // Reactors: online / standby / unconnected.
    for (e, role, status) in generators.iter() {
        let PowerRole::Generator { output, on } = *role else {
            continue;
        };
        let (ring, text) = if !status.ok() {
            (
                crate::power::PowerStatus::color(*status),
                format!("REACTOR — {}", status.label()),
            )
        } else if on {
            (
                Color::srgba(0.35, 1.0, 0.55, 0.8),
                format!("reactor {output} PU"),
            )
        } else {
            (
                Color::srgba(0.75, 0.75, 0.8, 0.6),
                "reactor standby".to_string(),
            )
        };
        for (v, mut sprite) in sprites.iter_mut() {
            if v.target == e && v.role == Role::FabRing {
                sprite.color = ring;
            }
        }
        for (v, mut t, _) in labels.iter_mut() {
            if v.target == e && v.role == Role::FabLabel {
                t.0 = text.clone();
            }
        }
    }
}

/// Power overlay: cable tiles colored per network, device rings by supply
/// status, rebuilt when (grid version, network states, device statuses)
/// change. Hidden entirely while the overlay is off.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn power_overlay_system(
    mut commands: Commands,
    map: Res<ShipMap>,
    art: Res<Art>,
    cables: Res<CableGrid>,
    state: Res<PowerState>,
    overlay: Res<PowerOverlay>,
    mut vis: ResMut<PowerOverlayVis>,
    devices: Query<(&Footprint, &PowerRole, &PowerStatus)>,
    mut root_q: Query<&mut Visibility, With<PowerOverlayRoot>>,
    children_q: Query<&Children>,
) {
    // Signature: anything that changes what the overlay should draw.
    let mut sig = if overlay.0 { cables.version } else { 0 };
    if overlay.0 {
        for n in &state.networks {
            sig = sig
                .wrapping_mul(31)
                .wrapping_add(n.generation as u64)
                .wrapping_add((n.demand as u64) << 8)
                .wrapping_add(n.generators as u64);
        }
        for (_, _, st) in devices.iter() {
            sig = sig.wrapping_mul(7).wrapping_add(*st as u64 + 1);
        }
    }
    if sig == vis.last_sig {
        return;
    }
    vis.last_sig = sig;
    if let Ok(children) = children_q.get(vis.root) {
        for &c in children {
            commands.entity(c).despawn();
        }
    }
    if !overlay.0 {
        if let Ok(mut v) = root_q.get_mut(vis.root) {
            *v = Visibility::Hidden;
        }
        return;
    }
    if let Ok(mut v) = root_q.get_mut(vis.root) {
        *v = Visibility::Visible;
    }
    let region_of = crate::power::flood_regions(&cables);
    // Distinct hue per network; dim red when the network has no generator.
    let net_color = |net: usize| -> Color {
        let powered = state.networks.get(net).is_some_and(|n| n.generators > 0);
        if powered {
            Color::hsl((net as f32 * 47.0) % 360.0, 0.75, 0.6)
        } else {
            Color::srgba(0.85, 0.3, 0.25, 0.85)
        }
    };
    for tile in cables.iter_cables() {
        let color = region_of
            .get(&tile)
            .map(|n| net_color(*n))
            .unwrap_or_else(|| Color::WHITE);
        commands
            .spawn((
                Sprite {
                    image: art.dot.clone(),
                    custom_size: Some(Vec2::splat(crate::TILE * 0.55)),
                    color,
                    ..default()
                },
                Transform::from_translation(map.world_pos(tile).extend(0.72)),
            ))
            .insert(ChildOf(vis.root));
    }
    for (foot, _, status) in devices.iter() {
        let p = foot_world_pos(foot);
        commands
            .spawn((
                Sprite {
                    image: art.ring.clone(),
                    custom_size: Some(Vec2::splat(crate::TILE * foot.w.max(foot.h) as f32 * 1.08)),
                    color: PowerStatus::color(*status),
                    ..default()
                },
                Transform::from_translation(p.extend(0.93)),
            ))
            .insert(ChildOf(vis.root));
    }
}

/// Selection ring, path preview dots, job target marker, hover ring and the
/// build-tool ghost.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn sync_selection_system(
    map: Res<ShipMap>,
    selection: Res<Selection>,
    hovered: Res<crate::input::Hovered>,
    markers: Res<Markers>,
    crews: Query<(Entity, &TilePos, &Movement, &CrewTask), With<Crew>>,
    items: Query<&TilePos, With<Item>>,
    racks: Query<(Entity, &TilePos), With<StorageCell>>,
    blueprints: Query<(Entity, &building::Footprint), With<Blueprint>>,
    buildings: Query<(Entity, &building::Footprint), (With<Building>, Without<Blueprint>)>,
    mut marker_q: Query<(&mut Transform, &mut Visibility)>,
) {
    // Hide everything first.
    for e in std::iter::once(markers.selection)
        .chain(std::iter::once(markers.hover))
        .chain(std::iter::once(markers.target))
        .chain(markers.dots.iter().copied())
    {
        if let Ok((_, mut vis)) = marker_q.get_mut(e) {
            *vis = Visibility::Hidden;
        }
    }

    let foot_pos = |e: Entity| {
        blueprints
            .iter()
            .find(|(be, _)| *be == e)
            .map(|(_, f)| foot_world_pos(f))
            .or_else(|| {
                buildings
                    .iter()
                    .find(|(be, _)| *be == e)
                    .map(|(_, f)| foot_world_pos(f))
            })
    };

    // Hover ring (white, softer than the selection ring).
    let hover_pos = match hovered.0 {
        Some(Selected::Crew(e)) => crews
            .get(e)
            .ok()
            .map(|(_, pos, mov, _)| crew_world_pos(&map, pos, mov)),
        Some(Selected::Item(e)) => items.get(e).ok().map(|p| map.world_pos(*p)),
        Some(Selected::Rack(e)) => racks.get(e).ok().map(|(_, p)| map.world_pos(*p)),
        Some(Selected::Blueprint(e)) | Some(Selected::Building(e)) => foot_pos(e),
        None => None,
    };
    if let Some(p) = hover_pos {
        if let Ok((mut tf, mut vis)) = marker_q.get_mut(markers.hover) {
            tf.translation = p.extend(0.85);
            *vis = Visibility::Visible;
        }
    }

    let Some(sel) = selection.0 else {
        return;
    };

    // Gather what to show without holding query borrows.
    let mut sel_pos: Option<Vec2> = None;
    let mut dot_pos: Vec<Vec2> = Vec::new();
    let mut target_pos: Option<Vec2> = None;
    match sel {
        Selected::Crew(e) => {
            if let Ok((_, pos, mov, task)) = crews.get(e) {
                sel_pos = Some(crew_world_pos(&map, pos, mov));
                dot_pos = mov.path.iter().map(|t| map.world_pos(*t)).collect();
                match task {
                    CrewTask::Haul(job) => {
                        let carrying =
                            matches!(job.phase, HaulPhase::ToDest | HaulPhase::Delivering);
                        target_pos = if carrying {
                            match job.dest {
                                HaulDest::Storage => job
                                    .target_rack
                                    .and_then(|r| racks.get(r).ok())
                                    .map(|(_, p)| map.world_pos(*p)),
                                HaulDest::Blueprint(bp) => foot_pos(bp),
                                HaulDest::Machine(m) => foot_pos(m),
                            }
                        } else {
                            items.get(job.item).ok().map(|p| map.world_pos(*p))
                        };
                    }
                    CrewTask::Build(job) | CrewTask::Deconstruct(job) | CrewTask::Operate(job) => {
                        target_pos = foot_pos(job.target);
                    }
                    CrewTask::Idle(_) => {}
                }
            }
        }
        Selected::Item(e) => {
            sel_pos = items.get(e).ok().map(|p| map.world_pos(*p));
        }
        Selected::Rack(e) => {
            sel_pos = racks.get(e).ok().map(|(_, p)| map.world_pos(*p));
        }
        Selected::Blueprint(e) | Selected::Building(e) => {
            sel_pos = foot_pos(e);
        }
    }

    if let Some(p) = sel_pos {
        if let Ok((mut tf, mut vis)) = marker_q.get_mut(markers.selection) {
            tf.translation = p.extend(0.9);
            *vis = Visibility::Visible;
        }
    }
    if let Some(p) = target_pos {
        if let Ok((mut tf, mut vis)) = marker_q.get_mut(markers.target) {
            tf.translation = p.extend(0.42);
            *vis = Visibility::Visible;
        }
    }
    for (i, p) in dot_pos.iter().enumerate() {
        if i >= markers.dots.len() {
            break;
        }
        if let Ok((mut tf, mut vis)) = marker_q.get_mut(markers.dots[i]) {
            tf.translation = p.extend(0.15);
            *vis = Visibility::Visible;
        }
    }
}

/// Placement ghost: follows the cursor while a build tool is active, green
/// when the footprint is placeable, red with the reason otherwise.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn ghost_system(
    map: Res<ShipMap>,
    art: Res<Art>,
    cables: Res<CableGrid>,
    build_mode: Res<BuildMode>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    ui: Query<&Interaction, With<Node>>,
    items: Query<(Entity, &TilePos), With<Item>>,
    blueprints: Query<(Entity, &building::Footprint), With<Blueprint>>,
    buildings: Query<(Entity, &building::Footprint), (With<Building>, Without<Blueprint>)>,
    markers: Res<Markers>,
    mut marker_q: Query<(&mut Transform, &mut Sprite, &mut Visibility), Without<Text2d>>,
    mut label_q: Query<(&mut Transform, &mut Text2d, &mut Visibility)>,
) {
    let over_ui = ui
        .iter()
        .any(|i| matches!(i, Interaction::Hovered | Interaction::Pressed));
    let mut hide = || {
        if let Ok((_, _, mut vis)) = marker_q.get_mut(markers.ghost) {
            *vis = Visibility::Hidden;
        }
        if let Ok((_, _, mut vis)) = label_q.get_mut(markers.ghost_label) {
            *vis = Visibility::Hidden;
        }
    };
    let Some(tool) = build_mode.0 else {
        hide();
        return;
    };
    let Some(cursor) = windows.single().ok().and_then(|w| w.cursor_position()) else {
        hide();
        return;
    };
    if over_ui {
        hide();
        return;
    }
    let Ok((cam, cam_gt)) = camera.single() else {
        hide();
        return;
    };
    let Ok(world) = cam.viewport_to_world_2d(cam_gt, cursor) else {
        hide();
        return;
    };
    let Some(tile) = map.tile_at_world(world) else {
        hide();
        return;
    };

    match tool {
        Tool::Build(kind) => {
            let d = building::def(kind);
            let foot = building::Footprint::new(tile.x, tile.y, d.w, d.h);
            let ground: Vec<TilePos> = items.iter().map(|(_, p)| *p).collect();
            let mut feet: Vec<(building::Footprint, bool)> =
                blueprints.iter().map(|(_, f)| (*f, true)).collect();
            feet.extend(buildings.iter().map(|(_, f)| (*f, false)));
            let check = building::can_place(&map, kind, tile, &ground, &feet, |p| cables.has(p));
            let (color, text) = match &check {
                Ok(()) => {
                    let cost: u32 = d.cost.iter().sum();
                    (
                        Color::srgba(0.4, 1.0, 0.5, 0.55),
                        format!("{} — {} part", d.label, cost),
                    )
                }
                Err(e) => (Color::srgba(1.0, 0.35, 0.3, 0.55), e.label().to_string()),
            };
            let p = foot_world_pos(&foot);
            if let Ok((mut tf, mut sprite, mut vis)) = marker_q.get_mut(markers.ghost) {
                sprite.image = art.building(kind).clone();
                sprite.custom_size = Some(Vec2::splat(crate::TILE * d.w as f32 * 0.95));
                sprite.color = color;
                tf.translation = p.extend(0.8);
                *vis = Visibility::Visible;
            }
            if let Ok((mut tf, mut text2d, mut vis)) = label_q.get_mut(markers.ghost_label) {
                text2d.0 = text;
                tf.translation = (p + Vec2::new(0.0, 14.0 + d.h as f32 * 10.0)).extend(0.82);
                *vis = Visibility::Visible;
            }
        }
        Tool::Deconstruct => {
            let found = buildings.iter().find(|(_, f)| f.contains(tile));
            let Some((_, f)) = found else {
                hide();
                return;
            };
            let p = foot_world_pos(f);
            if let Ok((mut tf, mut sprite, mut vis)) = marker_q.get_mut(markers.ghost) {
                sprite.image = art.ring.clone();
                sprite.custom_size = Some(Vec2::splat(crate::TILE * f.w.max(f.h) as f32 * 1.05));
                sprite.color = Color::srgba(1.0, 0.8, 0.25, 0.7);
                tf.translation = p.extend(0.8);
                *vis = Visibility::Visible;
            }
            if let Ok((mut tf, mut text2d, mut vis)) = label_q.get_mut(markers.ghost_label) {
                text2d.0 = "deconstruct".to_string();
                tf.translation = (p + Vec2::new(0.0, 14.0)).extend(0.82);
                *vis = Visibility::Visible;
            }
        }
    }
}

/// Despawn visuals whose target entity no longer exists.
fn cleanup_visuals_system(
    targets: Query<Entity, With<TilePos>>,
    visuals: Query<(Entity, &Visual)>,
    mut commands: Commands,
) {
    for (e, v) in visuals.iter() {
        if targets.get(v.target).is_err() {
            commands.entity(e).despawn();
        }
    }
}
