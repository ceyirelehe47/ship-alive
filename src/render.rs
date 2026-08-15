//! Rendering: every gameplay entity (crew, item, rack) is represented by a
//! separate "visual" entity linked through `Visual { target }`. Logic code
//! never touches sprites, and dead targets are cleaned up automatically, so
//! despawning an item can never leak or orphan its visuals.
//!
//! Art is loaded from `assets/art/*.png` when present; missing files fall
//! back to procedurally generated colored quads so the game stays playable
//! before the art pass (see `Art::load`).

use crate::crew::{Crew, CrewTask, HaulPhase, Movement};
use crate::input::Selection;
use crate::items::{CarriedBy, Item, ItemKind, MarkedForHaul, NoPathUntil, ReservedBy};
use crate::map::{ShipMap, TilePos};
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
}

#[derive(Component)]
pub struct Visual {
    pub target: Entity,
    pub role: Role,
}

/// Marker on logic entities that already have their visuals spawned.
#[derive(Component)]
pub struct HasVisual;

/// Persistent selection/path marker entities (pooled, hidden when unused).
#[derive(Resource)]
pub struct Markers {
    pub selection: Entity,
    pub hover: Entity,
    pub target: Entity,
    pub dots: Vec<Entity>,
}

/// All sprite/texture handles used by the game.
#[derive(Resource)]
pub struct Art {
    pub floor: Handle<Image>,
    pub wall: Handle<Image>,
    pub rack: Handle<Image>,
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
                    bevy::asset::RenderAssetUsages::MAIN_WORLD | bevy::asset::RenderAssetUsages::RENDER_WORLD,
                );
                images.add(img)
            }
        };
        Self {
            floor: fill("art/floor.png", [64, 70, 86, 255]),
            wall: fill("art/wall.png", [34, 38, 50, 255]),
            rack: fill("art/rack.png", [58, 118, 118, 255]),
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

pub struct RenderPlugin;

impl Plugin for RenderPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, (spawn_tile_visuals, spawn_room_labels, spawn_markers));
        app.add_systems(
            Update,
            (
                ensure_visuals_system,
                sync_crew_visuals_system,
                sync_item_visuals_system,
                sync_rack_labels_system,
                sync_selection_system,
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
            crate::map::Tile::Floor => art.floor.clone(),
            crate::map::Tile::Wall => art.wall.clone(),
        };
        let z = match tile {
            crate::map::Tile::Floor => 0.0,
            crate::map::Tile::Wall => 0.05,
        };
        commands.spawn(sprite(img, crate::TILE, z, map.world_pos(pos), Color::WHITE));
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
        ("HOLD B", 12, 10, 21, 17),
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
            Transform::from_translation((center + Vec2::new(0.0, size.y * 0.5 - 14.0)).extend(0.02)),
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
    commands.insert_resource(Markers { selection, hover, target, dots });
}

/// Spawn visuals for logic entities that do not have them yet.
#[allow(clippy::type_complexity)]
fn ensure_visuals_system(
    mut commands: Commands,
    map: Res<ShipMap>,
    art: Res<Art>,
    crews: Query<(Entity, &TilePos, &Crew), Without<HasVisual>>,
    items: Query<(Entity, &TilePos, &Item), Without<HasVisual>>,
    racks: Query<(Entity, &TilePos), (With<StorageCell>, Without<HasVisual>)>,
) {
    for (e, pos, item) in items.iter() {
        let p = map.world_pos(*pos);
        commands.spawn((
            Visual { target: e, role: Role::ItemSprite },
            sprite(art.item(item.kind).clone(), crate::TILE * 0.62, 0.35, p, Color::WHITE),
        ));
        commands.spawn((
            Visual { target: e, role: Role::ItemRing },
            sprite(art.ring.clone(), crate::TILE * 0.95, 0.45, p, Color::WHITE),
            Visibility::Hidden,
        ));
        commands.entity(e).insert(HasVisual);
    }
    for (e, pos, crew) in crews.iter() {
        let p = map.world_pos(*pos);
        commands.spawn((
            Visual { target: e, role: Role::CrewSprite },
            sprite(art.crew.clone(), crate::TILE * 0.8, 0.6, p, crew.tint),
        ));
        commands.spawn((
            Visual { target: e, role: Role::CrewLabel },
            Text2d::new(crew.name.clone()),
            TextFont {
                font_size: 11.0,
                ..default()
            },
            TextColor(crew.tint),
            Transform::from_translation((p + Vec2::new(0.0, -22.0)).extend(0.8)),
        ));
        commands.spawn((
            Visual { target: e, role: Role::CrewCarry },
            sprite(art.crate_.clone(), crate::TILE * 0.34, 0.7, p + Vec2::new(0.0, 24.0), Color::WHITE),
            Visibility::Hidden,
        ));
        commands.entity(e).insert(HasVisual);
    }
    for (e, pos) in racks.iter() {
        let p = map.world_pos(*pos);
        commands.spawn((
            Visual { target: e, role: Role::Rack },
            sprite(art.rack.clone(), crate::TILE * 0.95, 0.2, p, Color::WHITE),
        ));
        commands.spawn((
            Visual { target: e, role: Role::RackLabel },
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
}

/// Interpolated crew position (tile center → next tile center by progress).
fn crew_world_pos(map: &ShipMap, pos: &TilePos, mov: &Movement) -> Vec2 {
    let from = map.world_pos(*pos);
    if mov.path.is_empty() {
        from
    } else {
        from.lerp(map.world_pos(mov.path[0]), mov.progress.clamp(0.0, 1.0))
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
        } else if let Some(claimer_tint) = reserved.and_then(|r| {
            crews.iter().find(|(ce, _)| *ce == r.0).map(|(_, c)| c.tint)
        }) {
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
                    *vis = if carried_now { Visibility::Hidden } else { Visibility::Visible };
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
                text.0 = cell.label();
            }
        }
    }
}

/// Selection ring, path preview dots, job target marker and hover ring.
#[allow(clippy::type_complexity)]
fn sync_selection_system(
    map: Res<ShipMap>,
    selection: Res<Selection>,
    hovered: Res<crate::input::Hovered>,
    markers: Res<Markers>,
    crews: Query<(Entity, &TilePos, &Movement, &CrewTask), With<Crew>>,
    items: Query<&TilePos, With<Item>>,
    racks: Query<(Entity, &TilePos), With<StorageCell>>,
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

    // Hover ring (white, softer than the selection ring).
    let hover_pos = match hovered.0 {
        Some(crate::input::Selected::Crew(e)) => crews
            .get(e)
            .ok()
            .map(|(_, pos, mov, _)| crew_world_pos(&map, pos, mov)),
        Some(crate::input::Selected::Item(e)) => items.get(e).ok().map(|p| map.world_pos(*p)),
        Some(crate::input::Selected::Rack(e)) => racks.get(e).ok().map(|(_, p)| map.world_pos(*p)),
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
        crate::input::Selected::Crew(e) => {
            if let Ok((_, pos, mov, task)) = crews.get(e) {
                sel_pos = Some(crew_world_pos(&map, pos, mov));
                dot_pos = mov.path.iter().map(|t| map.world_pos(*t)).collect();
                if let CrewTask::Haul(job) = task {
                    let carrying = matches!(job.phase, HaulPhase::ToStorage | HaulPhase::Storing);
                    target_pos = if carrying {
                        job.target_rack
                            .and_then(|r| racks.get(r).ok())
                            .map(|(_, p)| map.world_pos(*p))
                    } else {
                        items.get(job.item).ok().map(|p| map.world_pos(*p))
                    };
                }
            }
        }
        crate::input::Selected::Item(e) => {
            sel_pos = items.get(e).ok().map(|p| map.world_pos(*p));
        }
        crate::input::Selected::Rack(e) => {
            sel_pos = racks.get(e).ok().map(|(_, p)| map.world_pos(*p));
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
