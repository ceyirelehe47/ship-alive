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
use crate::loc::{self, strings, Lang};
use crate::map::{ShipMap, TilePos};
use crate::power::{CableGrid, PowerRole, PowerState, PowerStatus};
use crate::production::{MachineState, RECIPE};
use crate::storage::StorageCell;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// Which sprite role a visual entity plays.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
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
    /// Door leaf sliding toward the negative end of the wall line.
    DoorLeafA,
    /// Door leaf sliding toward the positive end of the wall line.
    DoorLeafB,
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

/// Tags the thermal heat-map overlay root entity.
#[derive(Component)]
pub struct ThermalOverlayRoot;

/// Pooled heat-map tile sprite (one per open tile; colors updated in place).
#[derive(Component)]
pub struct HeatTile;

#[derive(Resource)]
pub struct ThermalOverlayVis {
    pub root: Entity,
    /// Membership signature: map tile set + thermal device set. A change
    /// rebuilds the pool (walls built/torn, devices placed/removed).
    pub last_sig: u64,
    /// Pool: one sprite per open tile, in `map.iter_tiles()` order.
    pub tiles: Vec<Entity>,
    /// Last color bucket drawn per pool slot.
    pub bucket: Vec<u16>,
    /// Device state rings, rebuilt on ring-signature change.
    pub rings: Vec<Entity>,
    pub ring_sig: u64,
    /// Wall-clock seconds since the last color refresh.
    pub refresh_acc: f32,
    /// Telemetry: pool rebuilds / sprite color writes since boot.
    pub rebuilds: u32,
    pub color_writes: u64,
}

/// Tags the coolant overlay root entity.
#[derive(Component)]
pub struct CoolantOverlayRoot;

/// Pooled coolant pipe dot (one per pipe tile; colors updated in place).
#[derive(Component)]
pub struct PipeDot;

/// Tags the compartment overlay root entity (Slice 4).
#[derive(Component)]
pub struct CompartmentOverlayRoot;

/// Pooled compartment tile sprite (one per interior tile incl. doors).
#[derive(Component)]
pub struct CompTile;

/// Tags the atmosphere overlay root entity (Slice 5).
#[derive(Component)]
pub struct AtmosphereOverlayRoot;

/// Tags the ventilation overlay root entity (Slice 6).
#[derive(Component)]
pub struct VentOverlayRoot;

/// Pooled duct tile sprite (one per duct cell).
#[derive(Component)]
pub struct DuctTile;

/// Pooled flow arrow (one per duct cell, visible only when flowing).
#[derive(Component)]
pub struct DuctFlow;

#[derive(Resource)]
pub struct VentOverlayVis {
    pub root: Entity,
    /// Membership signature: duct layout version + device counts.
    pub last_sig: u64,
    /// One sprite per duct cell, in `iter_ducts()` order.
    pub tiles: Vec<Entity>,
    /// One arrow per duct cell (same order).
    pub arrows: Vec<Entity>,
    /// Last color bucket per duct slot (quantized pressure | network).
    pub bucket: Vec<u32>,
    pub refresh_acc: f32,
    pub rebuilds: u32,
    pub color_writes: u64,
}

/// Pooled atmosphere tile sprite (one per gas tile incl. doors).
#[derive(Component)]
pub struct AtmoTile;

#[derive(Resource)]
pub struct AtmosphereOverlayVis {
    pub root: Entity,
    /// Membership signature: map geometry version.
    pub last_sig: u64,
    /// One sprite per gas tile, in `map.iter_tiles()` order.
    pub tiles: Vec<Entity>,
    /// Last color bucket per pool slot (quantized pressure | hazard | door |
    /// hover).
    pub bucket: Vec<u32>,
    /// "VENTING" warning labels at exposed-region centroids.
    pub labels: Vec<Entity>,
    pub refresh_acc: f32,
    pub rebuilds: u32,
    pub color_writes: u64,
}

#[derive(Resource)]
pub struct CompartmentOverlayVis {
    pub root: Entity,
    /// Membership signature: compartment geometry version.
    pub last_sig: u64,
    /// One sprite per interior tile, in `map.iter_tiles()` order.
    pub tiles: Vec<Entity>,
    /// Last color bucket per pool slot (region | hover | exposed | door).
    pub bucket: Vec<u32>,
    /// "EXPOSED" warning labels at exposed-region centroids.
    pub labels: Vec<Entity>,
    pub rebuilds: u32,
    pub color_writes: u64,
}

#[derive(Resource)]
pub struct CoolantOverlayVis {
    pub root: Entity,
    /// Membership signature: pipe layout + coolant device set.
    pub last_sig: u64,
    pub tiles: Vec<Entity>,
    /// Last color bucket per pool slot (temp<<8 | amount).
    pub bucket: Vec<u32>,
    pub rings: Vec<Entity>,
    pub ring_sig: u64,
    pub refresh_acc: f32,
    pub rebuilds: u32,
    pub color_writes: u64,
}

/// Overlay color refresh cadence (wall-clock seconds). Visual colors follow
/// the sim at this rate instead of every frame; membership edits (walls,
/// pipes, devices) still rebuild immediately.
const OVERLAY_REFRESH_SECS: f32 = 0.1;

/// target entity + role → visual entity. Maintained from `Added<Visual>` and
/// cleanup, so the per-frame sync systems update visuals with O(1) lookups
/// instead of scanning every visual for every target.
#[derive(Resource, Default)]
pub struct VisualIndex {
    map: std::collections::HashMap<(Entity, Role), Entity>,
}

impl VisualIndex {
    fn insert(&mut self, target: Entity, role: Role, visual: Entity) {
        self.map.insert((target, role), visual);
    }

    fn remove(&mut self, target: Entity, role: Role) {
        self.map.remove(&(target, role));
    }

    pub fn get(&self, target: Entity, role: Role) -> Option<Entity> {
        self.map.get(&(target, role)).copied()
    }
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
    pub pump: Handle<Image>,
    pub reservoir: Handle<Image>,
    pub heat_exchanger: Handle<Image>,
    pub radiator: Handle<Image>,
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
            BuildingKind::CoolantPipe => &self.dot,
            BuildingKind::Pump => &self.pump,
            BuildingKind::Reservoir => &self.reservoir,
            BuildingKind::HeatExchanger => &self.heat_exchanger,
            BuildingKind::Radiator => &self.radiator,
            BuildingKind::GasDuct => &self.dot,
            BuildingKind::Vent => &self.ring,
            BuildingKind::Blower => &self.pump,
            BuildingKind::GasTank => &self.reservoir,
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
            pump: fill("art/pump.png", [64, 168, 178, 255]),
            reservoir: fill("art/reservoir.png", [62, 104, 168, 255]),
            heat_exchanger: fill("art/heat_exchanger.png", [186, 142, 78, 255]),
            radiator: fill("art/radiator.png", [126, 134, 178, 255]),
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
        app.init_resource::<crate::OverlayMode>();
        app.init_resource::<VisualIndex>();
        app.add_systems(
            Startup,
            (spawn_tile_visuals, spawn_room_labels, spawn_markers),
        );
        app.add_systems(
            Update,
            (
                ensure_visuals_system,
                index_visuals_system,
                room_label_lang_system,
                sync_crew_visuals_system,
                sync_item_visuals_system,
                sync_rack_labels_system,
                sync_building_visuals_system,
                sync_door_visuals_system,
                sync_selection_system,
                ghost_system,
                power_overlay_system,
                thermal_overlay_system,
                coolant_overlay_system,
                compartment_overlay_system,
                atmosphere_overlay_system,
                ventilation_overlay_system,
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
fn spawn_room_labels(mut commands: Commands, lang: Res<Lang>) {
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
            Text2d::new(loc::room_label(name, strings(*lang))),
            RoomLabel(name),
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

/// A world-space room annotation keyed by its canonical English name
/// (localized on spawn and on language switches).
#[derive(Component)]
struct RoomLabel(&'static str);

/// Rewrite room annotations when the language changes.
fn room_label_lang_system(lang: Res<Lang>, mut q: Query<(&RoomLabel, &mut Text2d)>) {
    let l = strings(*lang);
    for (key, mut text) in q.iter_mut() {
        let want = loc::room_label(key.0, l);
        if text.0 != want {
            text.0 = want;
        }
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
    let thermal_root = commands
        .spawn((ThermalOverlayRoot, Transform::default(), Visibility::Hidden))
        .id();
    let coolant_root = commands
        .spawn((CoolantOverlayRoot, Transform::default(), Visibility::Hidden))
        .id();
    let compartment_root = commands
        .spawn((
            CompartmentOverlayRoot,
            Transform::default(),
            Visibility::Hidden,
        ))
        .id();
    let atmosphere_root = commands
        .spawn((
            AtmosphereOverlayRoot,
            Transform::default(),
            Visibility::Hidden,
        ))
        .id();
    let vent_root = commands
        .spawn((VentOverlayRoot, Transform::default(), Visibility::Hidden))
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
    commands.insert_resource(ThermalOverlayVis {
        root: thermal_root,
        last_sig: 0,
        tiles: Vec::new(),
        bucket: Vec::new(),
        rings: Vec::new(),
        ring_sig: 0,
        refresh_acc: 0.0,
        rebuilds: 0,
        color_writes: 0,
    });
    commands.insert_resource(CoolantOverlayVis {
        root: coolant_root,
        last_sig: 0,
        tiles: Vec::new(),
        bucket: Vec::new(),
        rings: Vec::new(),
        ring_sig: 0,
        refresh_acc: 0.0,
        rebuilds: 0,
        color_writes: 0,
    });
    commands.insert_resource(CompartmentOverlayVis {
        root: compartment_root,
        // u64::MAX forces the first activation to build the pool (the boot
        // geometry version is legitimately 0).
        last_sig: u64::MAX,
        tiles: Vec::new(),
        bucket: Vec::new(),
        labels: Vec::new(),
        rebuilds: 0,
        color_writes: 0,
    });
    commands.insert_resource(AtmosphereOverlayVis {
        root: atmosphere_root,
        last_sig: u64::MAX,
        tiles: Vec::new(),
        bucket: Vec::new(),
        labels: Vec::new(),
        refresh_acc: 0.0,
        rebuilds: 0,
        color_writes: 0,
    });
    commands.insert_resource(VentOverlayVis {
        root: vent_root,
        last_sig: u64::MAX,
        tiles: Vec::new(),
        arrows: Vec::new(),
        bucket: Vec::new(),
        refresh_acc: 0.0,
        rebuilds: 0,
        color_writes: 0,
    });
}

/// Fold newly spawned visuals into the `VisualIndex`.
fn index_visuals_system(
    mut index: ResMut<VisualIndex>,
    added: Query<(Entity, &Visual), Added<Visual>>,
) {
    for (e, v) in added.iter() {
        index.insert(v.target, v.role, e);
    }
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
        (
            Entity,
            &building::Footprint,
            &Building,
            Option<&crate::airtight::Door>,
        ),
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
    for (e, foot, b, door) in buildings.iter() {
        let p = foot_world_pos(foot);
        let d = building::def(b.kind);
        let size = crate::TILE * d.w as f32 * 0.98;
        if let Some(door) = door {
            // Doors render as two leaves. Closed, each leaf parks on its half
            // of the tile; the sync system slides them apart toward the walls
            // on either side as the door opens. Leaves sit just below wall z,
            // so an open leaf tucks behind the wall and only a thin frame
            // sliver stays visible in the doorway.
            let half = size * 0.5;
            let (a_off, b_off, b_flip) = match door.axis {
                // Wall line runs east-west: leaves park west/east.
                crate::airtight::DoorAxis::Ns => (
                    Vec2::new(-half * 0.5, 0.0),
                    Vec2::new(half * 0.5, 0.0),
                    (true, false),
                ),
                // Wall line runs north-south: leaves park south/north.
                crate::airtight::DoorAxis::Ew => (
                    Vec2::new(0.0, -half * 0.5),
                    Vec2::new(0.0, half * 0.5),
                    (false, true),
                ),
            };
            let (lw, lh) = match door.axis {
                crate::airtight::DoorAxis::Ns => (half, size),
                crate::airtight::DoorAxis::Ew => (size, half),
            };
            for (role, off, (fx, fy)) in [
                (Role::DoorLeafA, a_off, (false, false)),
                (Role::DoorLeafB, b_off, b_flip),
            ] {
                commands.spawn((
                    Visual { target: e, role },
                    Sprite {
                        image: art.building(b.kind).clone(),
                        custom_size: Some(Vec2::new(lw, lh)),
                        flip_x: fx,
                        flip_y: fy,
                        color: Color::WHITE,
                        ..default()
                    },
                    Transform::from_translation((p + off).extend(0.14)),
                ));
            }
            commands.entity(e).insert(HasVisual);
            continue;
        }
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
    index: Res<VisualIndex>,
    crews: Query<(Entity, &Crew, &CrewTask, &TilePos, &Movement)>,
    items: Query<(&CarriedBy, &Item)>,
    mut sprites: Query<(&mut Transform, &mut Sprite, &mut Visibility), Without<Text2d>>,
    mut labels: Query<&mut Transform, With<Text2d>>,
) {
    // One pass over items instead of a find() per crew.
    let mut carried_kind: std::collections::HashMap<Entity, ItemKind> =
        std::collections::HashMap::new();
    for (c, i) in items.iter() {
        carried_kind.insert(c.0, i.kind);
    }
    for (e, crew, task, pos, mov) in crews.iter() {
        let p = crew_world_pos(&map, pos, mov);
        let idle = matches!(task, CrewTask::Idle(_));
        if let Some(ve) = index.get(e, Role::CrewLabel) {
            if let Ok(mut tf) = labels.get_mut(ve) {
                tf.translation = (p + Vec2::new(0.0, -22.0)).extend(0.8);
            }
        }
        if let Some(ve) = index.get(e, Role::CrewSprite) {
            if let Ok((mut tf, mut sprite, _)) = sprites.get_mut(ve) {
                tf.translation = p.extend(0.6);
                let want = if idle { dimmed(crew.tint) } else { crew.tint };
                if sprite.color != want {
                    sprite.color = want;
                }
            }
        }
        if let Some(ve) = index.get(e, Role::CrewCarry) {
            if let Ok((mut tf, mut sprite, mut vis)) = sprites.get_mut(ve) {
                if let Some(kind) = carried_kind.get(&e) {
                    tf.translation = (p + Vec2::new(0.0, 24.0)).extend(0.7);
                    sprite.image = art.item(*kind).clone();
                    *vis = Visibility::Visible;
                } else {
                    *vis = Visibility::Hidden;
                }
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
    clock: Res<crate::simtime::SimClock>,
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
    index: Res<VisualIndex>,
    mut sprites: Query<(&mut Transform, &mut Sprite, &mut Visibility), Without<Text2d>>,
) {
    let now = clock.now();
    // Claimer tints once per frame instead of a find() per item.
    let tints: std::collections::HashMap<Entity, Color> =
        crews.iter().map(|(e, c)| (e, c.tint)).collect();
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
        } else if let Some(claimer_tint) = reserved.and_then(|r| tints.get(&r.0).copied()) {
            claimer_tint
        } else {
            Color::WHITE
        };
        if let Some(ve) = index.get(e, Role::ItemSprite) {
            if let Ok((mut tf, _, mut vis)) = sprites.get_mut(ve) {
                let want_vis = if carried_now {
                    Visibility::Hidden
                } else {
                    Visibility::Visible
                };
                if *vis != want_vis {
                    *vis = want_vis;
                }
                let want = p.extend(0.35);
                if tf.translation != want {
                    tf.translation = want;
                }
            }
        }
        if let Some(ve) = index.get(e, Role::ItemRing) {
            if let Ok((mut tf, mut sprite, mut vis)) = sprites.get_mut(ve) {
                let want_vis = if marked.is_some() && !carried_now {
                    Visibility::Visible
                } else {
                    Visibility::Hidden
                };
                if *vis != want_vis {
                    *vis = want_vis;
                }
                if sprite.color != ring_color {
                    sprite.color = ring_color;
                }
                let want = p.extend(0.45);
                if tf.translation != want {
                    tf.translation = want;
                }
            }
        }
    }
}

/// Rack count labels.
fn sync_rack_labels_system(
    index: Res<VisualIndex>,
    racks: Query<(Entity, &StorageCell), Changed<StorageCell>>,
    mut labels: Query<&mut Text2d>,
) {
    for (e, cell) in racks.iter() {
        if let Some(ve) = index.get(e, Role::RackLabel) {
            if let Ok(mut text) = labels.get_mut(ve) {
                let want = format!("{} {}", cell.label(), cell.filter_label());
                if text.0 != want {
                    text.0 = want;
                }
            }
        }
    }
}

/// Building & blueprint visuals: deconstruct tint + progress, blueprint
/// materials/progress text, fabricator state ring and label.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn sync_building_visuals_system(
    blueprints: Query<(Entity, &Blueprint), Changed<Blueprint>>,
    buildings: Query<(Entity, &Building, Option<&MarkedForDeconstruct>), Changed<Building>>,
    fabs: Query<(
        Entity,
        &crate::production::Fabricator,
        &crate::power::PowerStatus,
    )>,
    generators: Query<(Entity, &crate::power::PowerRole, &crate::power::PowerStatus)>,
    lang: Res<Lang>,
    index: Res<VisualIndex>,
    mut sprites: Query<&mut Sprite, Without<Text2d>>,
    mut labels: Query<(&mut Text2d, &mut TextColor)>,
) {
    let l = strings(*lang);
    for (e, bp) in blueprints.iter() {
        let label = if bp.progress > 0.0 {
            format!("{}%", (bp.progress * 100.0) as u32)
        } else {
            bp.materials_label_loc(l)
        };
        if let Some(ve) = index.get(e, Role::BlueprintLabel) {
            if let Ok((mut text, _)) = labels.get_mut(ve) {
                if text.0 != label {
                    text.0 = label;
                }
            }
        }
    }
    for (e, _, marked) in buildings.iter() {
        if let Some(ve) = index.get(e, Role::BuildingSprite) {
            if let Ok(mut sprite) = sprites.get_mut(ve) {
                let want = if marked.is_some() {
                    Color::srgb(1.0, 0.8, 0.25)
                } else {
                    Color::WHITE
                };
                if sprite.color != want {
                    sprite.color = want;
                }
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
                crate::tfmt!(l.lbl_no_power, status = loc::power_status_label(*power, l)),
                Color::srgb(1.0, 0.55, 0.45),
            )
        } else {
            match state {
                MachineState::NoOrder => (
                    Color::srgba(0.6, 0.65, 0.7, 0.35),
                    l.lbl_no_order.to_string(),
                    Color::WHITE,
                ),
                MachineState::WaitingInput => (
                    Color::srgba(1.0, 0.75, 0.25, 0.6),
                    crate::tfmt!(l.fmt_lbl_need_ore, n = RECIPE.in_qty),
                    Color::WHITE,
                ),
                MachineState::WaitingWorker => (
                    Color::srgba(0.35, 0.75, 1.0, 0.6),
                    l.lbl_wait_worker.to_string(),
                    Color::WHITE,
                ),
                MachineState::Working => (
                    Color::srgba(0.35, 1.0, 0.5, 0.7),
                    crate::tfmt!(l.fmt_lbl_working, p = (f.progress * 100.0) as u32),
                    Color::srgb(0.55, 1.0, 0.65),
                ),
                MachineState::OutputBlocked => (
                    Color::srgba(1.0, 0.35, 0.3, 0.7),
                    l.lbl_blocked.to_string(),
                    Color::srgb(1.0, 0.5, 0.4),
                ),
            }
        };
        if let Some(ve) = index.get(e, Role::FabRing) {
            if let Ok(mut sprite) = sprites.get_mut(ve) {
                if sprite.color != ring {
                    sprite.color = ring;
                }
            }
        }
        if let Some(ve) = index.get(e, Role::FabLabel) {
            if let Ok((mut t, mut c)) = labels.get_mut(ve) {
                if t.0 != text {
                    t.0 = text;
                }
                if c.0 != text_color {
                    c.0 = text_color;
                }
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
                crate::tfmt!(
                    l.fmt_lbl_reactor_bad,
                    status = loc::power_status_label(*status, l)
                ),
            )
        } else if on {
            (
                Color::srgba(0.35, 1.0, 0.55, 0.8),
                crate::tfmt!(l.fmt_lbl_reactor_out, out = output),
            )
        } else {
            (
                Color::srgba(0.75, 0.75, 0.8, 0.6),
                l.lbl_reactor_standby.to_string(),
            )
        };
        if let Some(ve) = index.get(e, Role::FabRing) {
            if let Ok(mut sprite) = sprites.get_mut(ve) {
                if sprite.color != ring {
                    sprite.color = ring;
                }
            }
        }
        if let Some(ve) = index.get(e, Role::FabLabel) {
            if let Ok((mut t, _)) = labels.get_mut(ve) {
                if t.0 != text {
                    t.0 = text;
                }
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
    overlay: Res<crate::OverlayMode>,
    mut vis: ResMut<PowerOverlayVis>,
    devices: Query<(&Footprint, &PowerRole, &PowerStatus)>,
    mut root_q: Query<&mut Visibility, With<PowerOverlayRoot>>,
    children_q: Query<&Children>,
) {
    // Signature: anything that changes what the overlay should draw.
    let mut sig = if *overlay == crate::OverlayMode::Power {
        cables.version
    } else {
        0
    };
    if *overlay == crate::OverlayMode::Power {
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
    if *overlay != crate::OverlayMode::Power {
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

/// Thermal heat map: one pooled sprite per open tile whose color follows the
/// ambient temperature. The pool is rebuilt only when the tile set or device
/// set changes (walls built/torn, devices placed); afterwards colors are
/// updated in place at `OVERLAY_REFRESH_SECS` cadence, writing only the
/// sprites whose 1 °C temperature bucket changed.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn thermal_overlay_system(
    mut commands: Commands,
    // Real time: the refresh cadence is wall-clock by design (the virtual
    // clock runs at BASE_SIM_RATE × game speed and pauses with the game).
    time: Res<Time<Real>>,
    map: Res<ShipMap>,
    art: Res<Art>,
    grid: Res<crate::thermal::ThermalGrid>,
    overlay: Res<crate::OverlayMode>,
    mut vis: ResMut<ThermalOverlayVis>,
    devices: Query<(Entity, &Footprint, &crate::thermal::ThermalState)>,
    mut root_q: Query<&mut Visibility, With<ThermalOverlayRoot>>,
    children_q: Query<&Children>,
    mut tile_q: Query<&mut Sprite, With<HeatTile>>,
) {
    let active = *overlay == crate::OverlayMode::Thermal;
    if let Ok(mut v) = root_q.get_mut(vis.root) {
        let want = if active {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *v != want {
            *v = want;
        }
    }
    if !active {
        return;
    }

    // Membership signature: tile set + thermal device set.
    let mut sig = map.version;
    for (e, _, _) in devices.iter() {
        sig = sig.wrapping_mul(31).wrapping_add(e.to_bits());
    }
    if sig != vis.last_sig {
        vis.last_sig = sig;
        vis.rebuilds += 1;
        if let Ok(children) = children_q.get(vis.root) {
            for &c in children {
                commands.entity(c).despawn();
            }
        }
        vis.tiles.clear();
        vis.bucket.clear();
        vis.ring_sig = u64::MAX; // rings were despawned with the pool
        for (pos, tile) in map.iter_tiles() {
            if matches!(tile, crate::map::Tile::Wall | crate::map::Tile::BuiltWall) {
                continue;
            }
            let t = grid.amb_at(pos);
            let id = commands
                .spawn((
                    HeatTile,
                    Sprite {
                        image: art.floor.clone(),
                        custom_size: Some(Vec2::splat(crate::TILE)),
                        color: heat_tile_color(t),
                        ..default()
                    },
                    Transform::from_translation(map.world_pos(pos).extend(0.70)),
                ))
                .insert(ChildOf(vis.root))
                .id();
            vis.tiles.push(id);
            vis.bucket.push(temp_bucket(t));
        }
        // Spawned entities appear at the next command flush; paint then.
        vis.refresh_acc = OVERLAY_REFRESH_SECS;
    }

    // Device state rings (few entities — cheap to rebuild on change).
    let mut ring_sig: u64 = 1;
    for (e, _, state) in devices.iter() {
        ring_sig = ring_sig
            .wrapping_mul(31)
            .wrapping_add(e.to_bits() * 8 + *state as u64);
    }
    if ring_sig != vis.ring_sig {
        vis.ring_sig = ring_sig;
        for old in vis.rings.drain(..) {
            commands.entity(old).despawn();
        }
        for (_, foot, state) in devices.iter() {
            let p = foot_world_pos(foot);
            let id = commands
                .spawn((
                    Sprite {
                        image: art.ring.clone(),
                        custom_size: Some(Vec2::splat(
                            crate::TILE * foot.w.max(foot.h) as f32 * 1.08,
                        )),
                        color: state.color(),
                        ..default()
                    },
                    Transform::from_translation(p.extend(0.93)),
                ))
                .insert(ChildOf(vis.root))
                .id();
            vis.rings.push(id);
        }
    }

    vis.refresh_acc += time.delta_secs();
    if vis.refresh_acc < OVERLAY_REFRESH_SECS {
        return;
    }
    vis.refresh_acc = 0.0;
    let open_tiles = map
        .iter_tiles()
        .filter(|(_, tile)| !matches!(tile, crate::map::Tile::Wall | crate::map::Tile::BuiltWall));
    for (ti, (pos, _)) in open_tiles.enumerate() {
        let t = grid.amb_at(pos);
        let b = temp_bucket(t);
        if b != vis.bucket[ti] {
            vis.bucket[ti] = b;
            if let Ok(mut sprite) = tile_q.get_mut(vis.tiles[ti]) {
                sprite.color = heat_tile_color(t);
                vis.color_writes += 1;
            }
        }
    }
}

/// Heat-map tile color (shared heat ramp at 40% alpha).
fn heat_tile_color(t: f32) -> Color {
    let mut c = crate::thermal::heat_color(t);
    if let Color::Srgba(ref mut s) = c {
        s.alpha = 0.40;
    }
    c
}

/// 1 °C color buckets: slow drift repaints only tiles that crossed a degree.
fn temp_bucket(t: f32) -> u16 {
    ((t + 100.0).round().clamp(0.0, 500.0)) as u16
}

/// Coolant overlay: one pooled dot per pipe tile tinted by water temperature
/// (alpha tracks how full the tile is), rings on pump / exchanger / radiator
/// devices. Pool rebuilt only on pipe/device edits; colors refreshed in place
/// at `OVERLAY_REFRESH_SECS` cadence, writing only changed dots.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn coolant_overlay_system(
    mut commands: Commands,
    // Real time: the refresh cadence is wall-clock by design (the virtual
    // clock runs at BASE_SIM_RATE × game speed and pauses with the game).
    time: Res<Time<Real>>,
    map: Res<ShipMap>,
    art: Res<Art>,
    pipes: Res<crate::coolant::PipeGrid>,
    water: Res<crate::coolant::WaterGrid>,
    overlay: Res<crate::OverlayMode>,
    mut vis: ResMut<CoolantOverlayVis>,
    devices: Query<(
        Entity,
        &Footprint,
        &Building,
        Option<&crate::power::PowerStatus>,
    )>,
    pumps: Query<(Entity, &Footprint, &crate::power::PowerStatus), With<crate::coolant::Pump>>,
    mut root_q: Query<&mut Visibility, With<CoolantOverlayRoot>>,
    children_q: Query<&Children>,
    mut dot_q: Query<&mut Sprite, With<PipeDot>>,
) {
    let active = *overlay == crate::OverlayMode::Coolant;
    if let Ok(mut v) = root_q.get_mut(vis.root) {
        let want = if active {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *v != want {
            *v = want;
        }
    }
    if !active {
        return;
    }

    // Membership signature: pipe layout + coolant device set.
    let coolant_kinds = |b: &Building| {
        matches!(
            b.kind,
            BuildingKind::Pump
                | BuildingKind::Reservoir
                | BuildingKind::HeatExchanger
                | BuildingKind::Radiator
        )
    };
    let mut sig = pipes.version;
    for (e, _, b, _) in devices.iter() {
        if coolant_kinds(b) {
            sig = sig.wrapping_mul(31).wrapping_add(e.to_bits());
        }
    }
    if sig != vis.last_sig {
        vis.last_sig = sig;
        vis.rebuilds += 1;
        if let Ok(children) = children_q.get(vis.root) {
            for &c in children {
                commands.entity(c).despawn();
            }
        }
        vis.tiles.clear();
        vis.bucket.clear();
        vis.ring_sig = u64::MAX; // rings were despawned with the pool
        for tile in pipes.iter_pipes() {
            let (amount, temp) = (water.amount_at(tile), water.temp_at(tile));
            let id = commands
                .spawn((
                    PipeDot,
                    Sprite {
                        image: art.dot.clone(),
                        custom_size: Some(Vec2::splat(crate::TILE * 0.6)),
                        color: pipe_dot_color(amount, temp),
                        ..default()
                    },
                    Transform::from_translation(map.world_pos(tile).extend(0.72)),
                ))
                .insert(ChildOf(vis.root))
                .id();
            vis.tiles.push(id);
            vis.bucket.push(pipe_bucket(amount, temp));
        }
        vis.refresh_acc = OVERLAY_REFRESH_SECS;
    }

    // Device rings + powered-pump double rings.
    let mut ring_sig: u64 = 1;
    for (e, _, b, st) in devices.iter() {
        if coolant_kinds(b) {
            ring_sig = ring_sig
                .wrapping_mul(31)
                .wrapping_add(e.to_bits() * 32 + st.map(|s| *s as u64).unwrap_or(9));
        }
    }
    for (e, _, st) in pumps.iter() {
        ring_sig = ring_sig
            .wrapping_mul(31)
            .wrapping_add(e.to_bits() * 32 + *st as u64 + 17);
    }
    if ring_sig != vis.ring_sig {
        vis.ring_sig = ring_sig;
        for old in vis.rings.drain(..) {
            commands.entity(old).despawn();
        }
        for (_, foot, b, status) in devices.iter() {
            if !coolant_kinds(b) {
                continue;
            }
            let p = foot_world_pos(foot);
            let color = status
                .map(|s| s.color())
                .unwrap_or(Color::srgba(0.6, 0.8, 0.9, 0.9));
            let id = commands
                .spawn((
                    Sprite {
                        image: art.ring.clone(),
                        custom_size: Some(Vec2::splat(crate::TILE * 1.08)),
                        color,
                        ..default()
                    },
                    Transform::from_translation(p.extend(0.93)),
                ))
                .insert(ChildOf(vis.root))
                .id();
            vis.rings.push(id);
        }
        for (_, foot, status) in pumps.iter() {
            let p = foot_world_pos(foot);
            let id = commands
                .spawn((
                    Sprite {
                        image: art.ring.clone(),
                        custom_size: Some(Vec2::splat(crate::TILE * 1.18)),
                        color: status.color(),
                        ..default()
                    },
                    Transform::from_translation(p.extend(0.94)),
                ))
                .insert(ChildOf(vis.root))
                .id();
            vis.rings.push(id);
        }
    }

    vis.refresh_acc += time.delta_secs();
    if vis.refresh_acc < OVERLAY_REFRESH_SECS {
        return;
    }
    vis.refresh_acc = 0.0;
    for (ti, tile) in pipes.iter_pipes().enumerate() {
        let (amount, temp) = (water.amount_at(tile), water.temp_at(tile));
        let b = pipe_bucket(amount, temp);
        if b != vis.bucket[ti] {
            vis.bucket[ti] = b;
            if let Ok(mut sprite) = dot_q.get_mut(vis.tiles[ti]) {
                sprite.color = pipe_dot_color(amount, temp);
                vis.color_writes += 1;
            }
        }
    }
}

/// Pipe dot color: heat ramp clamped to 15..70 °C, alpha by fill.
fn pipe_dot_color(amount: f32, temp: f32) -> Color {
    let mut c = crate::thermal::heat_color(temp.clamp(0.0, 75.0));
    if let Color::Srgba(ref mut s) = c {
        s.alpha = if amount <= 0.0 {
            0.12
        } else {
            (0.25 + 0.55 * (amount / crate::coolant::PIPE_TILE_CAP).min(1.0)).min(0.85)
        };
    }
    c
}

/// Color bucket for a pipe dot: temperature (1 °C) and amount (¼ unit).
fn pipe_bucket(amount: f32, temp: f32) -> u32 {
    let t = temp.round().clamp(0.0, 255.0) as u32;
    let a = ((amount * 4.0).round().clamp(0.0, 255.0)) as u32;
    (t << 8) | a
}

// =====================================================================================
// Slice 4: doors & compartments
// =====================================================================================

/// Normal-view door readability: the two door leaves slide apart toward the
/// walls on either side of the doorway as it opens (leaf sprites sit just
/// below wall z, so open leaves tuck behind the walls and only thin frame
/// slivers remain in the doorway). Locked doors take a red tint, doors marked
/// for deconstruction a yellow one.
fn sync_door_visuals_system(
    map: Res<ShipMap>,
    index: Res<VisualIndex>,
    doors: Query<(
        Entity,
        &TilePos,
        &crate::airtight::Door,
        Option<&MarkedForDeconstruct>,
    )>,
    mut leaves: Query<(&mut Transform, &mut Sprite), Without<Text2d>>,
) {
    for (e, pos, door, marked) in doors.iter() {
        let (Some(la), Some(lb)) = (index.get(e, Role::DoorLeafA), index.get(e, Role::DoorLeafB))
        else {
            continue;
        };
        let p = map.world_pos(*pos);
        let base = crate::TILE * 0.98;
        // Closed: each leaf centered on its half of the tile. Fully open:
        // slid outward until only a 12% sliver still covers the doorway.
        let at = base * 0.25 + base * 0.38 * door.progress;
        let (a, b) = match door.axis {
            // Wall line runs east-west: the leaves slide along X.
            crate::airtight::DoorAxis::Ns => (p + Vec2::new(-at, 0.0), p + Vec2::new(at, 0.0)),
            // Wall line runs north-south: the leaves slide along Y.
            crate::airtight::DoorAxis::Ew => (p + Vec2::new(0.0, -at), p + Vec2::new(0.0, at)),
        };
        let color = if marked.is_some() {
            Color::srgb(1.0, 0.8, 0.25)
        } else if door.mode == crate::airtight::DoorMode::LockClosed {
            Color::srgb(1.0, 0.45, 0.4)
        } else if door.progress >= 1.0 {
            Color::srgba(0.75, 0.95, 1.0, 0.45)
        } else {
            Color::WHITE
        };
        for (ve, want) in [(la, a), (lb, b)] {
            if let Ok((mut tr, mut sp)) = leaves.get_mut(ve) {
                tr.translation = want.extend(0.14);
                sp.color = color;
            }
        }
    }
}

/// Stable per-region hue (same topology ⇒ same colors; region ids are scan
/// order, so only geometry edits can renumber them).
fn region_color(region: u16, exposed: bool, hovered: bool) -> Color {
    if exposed {
        return if hovered {
            Color::srgba(1.0, 0.42, 0.22, 0.62)
        } else {
            Color::srgba(1.0, 0.35, 0.2, 0.45)
        };
    }
    let mut c = Color::hsl((region as f32 * 47.0) % 360.0, 0.75, 0.62);
    if let Color::Hsla(ref mut h) = c {
        h.alpha = if hovered { 0.68 } else { 0.48 };
    }
    c
}

/// Door tiles inside the overlay: sealed = red barrier, open = green link.
fn door_overlay_color(open: bool, hovered: bool) -> Color {
    if open {
        Color::srgba(0.35, 1.0, 0.5, if hovered { 0.95 } else { 0.8 })
    } else {
        Color::srgba(0.95, 0.3, 0.28, if hovered { 0.95 } else { 0.85 })
    }
}

/// Compartment / airtight overlay: one pooled sprite per interior tile,
/// tinted by structural compartment (stable hues), doors drawn as red
/// barriers / green links, exposed regions flashing a warning color, and the
/// hovered compartment brightened so its extent reads at a glance. Colors
/// are static per topology, so the refresh is a bucket compare per tile with
/// writes only on actual change (hover moves repaint two compartments).
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn compartment_overlay_system(
    mut commands: Commands,
    map: Res<ShipMap>,
    art: Res<Art>,
    lang: Res<Lang>,
    comps: Res<crate::airtight::Compartments>,
    overlay: Res<crate::OverlayMode>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    mut vis: ResMut<CompartmentOverlayVis>,
    mut root_q: Query<&mut Visibility, With<CompartmentOverlayRoot>>,
    children_q: Query<&Children>,
    mut tile_q: Query<&mut Sprite, With<CompTile>>,
) {
    let active = *overlay == crate::OverlayMode::Compartments;
    if let Ok(mut v) = root_q.get_mut(vis.root) {
        let want = if active {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *v != want {
            *v = want;
        }
    }
    if !active {
        return;
    }

    // Hovered tile → hovered compartment (pure cursor→tile mapping; entities
    // not involved — plain floor counts too).
    let hovered_tile = (|| {
        let cursor = windows.single().ok()?.cursor_position()?;
        let (cam, cam_gt) = camera.single().ok()?;
        let world = cam.viewport_to_world_2d(cam_gt, cursor).ok()?;
        map.tile_at_world(world)
    })()
    .unwrap_or(TilePos::new(-1, -1));
    let hovered_region = comps.region_at(hovered_tile);

    // Membership signature: compartment geometry version.
    let sig = comps.geometry_version;
    if sig != vis.last_sig {
        vis.last_sig = sig;
        vis.rebuilds += 1;
        if let Ok(children) = children_q.get(vis.root) {
            for &c in children {
                commands.entity(c).despawn();
            }
        }
        vis.tiles.clear();
        vis.bucket.clear();
        for (pos, tile) in map.iter_tiles() {
            if matches!(tile, crate::map::Tile::Wall | crate::map::Tile::BuiltWall) {
                continue;
            }
            let id = commands
                .spawn((
                    CompTile,
                    Sprite {
                        image: art.floor.clone(),
                        custom_size: Some(Vec2::splat(crate::TILE)),
                        color: Color::WHITE,
                        ..default()
                    },
                    Transform::from_translation(map.world_pos(pos).extend(0.71)),
                ))
                .insert(ChildOf(vis.root))
                .id();
            vis.tiles.push(id);
            vis.bucket.push(u32::MAX);
        }
        vis.labels.clear();
        for r in &comps.regions {
            if !r.exposed {
                continue;
            }
            let id = commands
                .spawn((
                    Text2d::new(strings(*lang).lbl_exposed_space),
                    RoomLabel("EXPOSED TO SPACE"),
                    TextFont {
                        font_size: 13.0,
                        ..default()
                    },
                    TextColor(Color::srgb(1.0, 0.5, 0.35)),
                    Transform::from_translation(map.world_pos(r.centroid).extend(0.95)),
                ))
                .insert(ChildOf(vis.root))
                .id();
            vis.labels.push(id);
        }
    }

    // Per-tile color bucket compare (region | hover | exposed | door state).
    let interior: Vec<(TilePos, crate::map::Tile)> = map
        .iter_tiles()
        .filter(|(_, t)| !matches!(t, crate::map::Tile::Wall | crate::map::Tile::BuiltWall))
        .collect();
    for (ti, (pos, tile)) in interior.iter().enumerate() {
        let door = *tile == crate::map::Tile::Door;
        let region = comps.region_at(*pos);
        let exposed = region != crate::airtight::NO_REGION
            && comps
                .regions
                .get(region as usize)
                .is_some_and(|r| r.exposed);
        let hovered = hovered_region != crate::airtight::NO_REGION && region == hovered_region;
        let door_open = door && map.door_state(*pos).is_some_and(|d| d.open >= 1.0);
        let bucket = (region as u32 & 0x3FFF)
            | (u32::from(hovered) << 14)
            | (u32::from(exposed) << 15)
            | (u32::from(door) << 16)
            | (u32::from(door_open) << 17);
        if bucket != vis.bucket[ti] {
            vis.bucket[ti] = bucket;
            let color = if door {
                door_overlay_color(door_open, hovered)
            } else {
                region_color(region, exposed, hovered)
            };
            if let Ok(mut sprite) = tile_q.get_mut(vis.tiles[ti]) {
                sprite.color = color;
                vis.color_writes += 1;
            }
        }
    }
}

/// Atmosphere / pressure overlay (Slice 5): one pooled sprite per gas tile,
/// tinted by derived pressure (vacuum dark → low blue → normal green → high
/// yellow/red), with composition hazards overriding in warning colors, doors
/// keeping the sealed-red / open-green convention, and the hovered tile
/// brightened. Pressure is quantized into color buckets, so the refresh is a
/// bucket compare per tile on a wall-clock cadence with writes only on
/// actual change — no per-frame repaint of a static ship.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn atmosphere_overlay_system(
    mut commands: Commands,
    real: Res<Time<Real>>,
    map: Res<ShipMap>,
    art: Res<Art>,
    lang: Res<Lang>,
    atmo: Res<crate::atmosphere::AtmosphereGrid>,
    thermal: Res<crate::thermal::ThermalGrid>,
    comps: Res<crate::airtight::Compartments>,
    overlay: Res<crate::OverlayMode>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    mut vis: ResMut<AtmosphereOverlayVis>,
    mut root_q: Query<&mut Visibility, With<AtmosphereOverlayRoot>>,
    children_q: Query<&Children>,
    mut tile_q: Query<&mut Sprite, With<AtmoTile>>,
) {
    let active = *overlay == crate::OverlayMode::Atmosphere;
    if let Ok(mut v) = root_q.get_mut(vis.root) {
        let want = if active {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *v != want {
            *v = want;
        }
    }
    if !active {
        return;
    }

    // Hovered tile (same cursor→tile mapping as the compartment overlay).
    let hovered_tile = (|| {
        let cursor = windows.single().ok()?.cursor_position()?;
        let (cam, cam_gt) = camera.single().ok()?;
        let world = cam.viewport_to_world_2d(cam_gt, cursor).ok()?;
        map.tile_at_world(world)
    })()
    .unwrap_or(TilePos::new(-1, -1));

    // Membership signature: geometry version.
    let sig = map.version;
    if sig != vis.last_sig {
        vis.last_sig = sig;
        vis.rebuilds += 1;
        if let Ok(children) = children_q.get(vis.root) {
            for &c in children {
                commands.entity(c).despawn();
            }
        }
        vis.tiles.clear();
        vis.bucket.clear();
        for (pos, tile) in map.iter_tiles() {
            if matches!(tile, crate::map::Tile::Wall | crate::map::Tile::BuiltWall) {
                continue;
            }
            let id = commands
                .spawn((
                    AtmoTile,
                    Sprite {
                        image: art.floor.clone(),
                        custom_size: Some(Vec2::splat(crate::TILE)),
                        color: Color::WHITE,
                        ..default()
                    },
                    Transform::from_translation(map.world_pos(pos).extend(0.71)),
                ))
                .insert(ChildOf(vis.root))
                .id();
            vis.tiles.push(id);
            vis.bucket.push(u32::MAX);
        }
        vis.labels.clear();
        for r in &comps.regions {
            if !r.exposed {
                continue;
            }
            let id = commands
                .spawn((
                    Text2d::new(strings(*lang).alert_venting_space),
                    RoomLabel("VENTING TO SPACE"),
                    TextFont {
                        font_size: 13.0,
                        ..default()
                    },
                    TextColor(Color::srgb(1.0, 0.45, 0.3)),
                    Transform::from_translation(map.world_pos(r.centroid).extend(0.95)),
                ))
                .insert(ChildOf(vis.root))
                .id();
            vis.labels.push(id);
        }
    }

    // Color cadence (visual follows the sim at 10 Hz, not per frame).
    vis.refresh_acc += real.delta_secs();
    if vis.refresh_acc < OVERLAY_REFRESH_SECS {
        return;
    }
    vis.refresh_acc = 0.0;

    let interior: Vec<(TilePos, crate::map::Tile)> = map
        .iter_tiles()
        .filter(|(_, t)| !matches!(t, crate::map::Tile::Wall | crate::map::Tile::BuiltWall))
        .collect();
    for (ti, (pos, tile)) in interior.iter().enumerate() {
        let door = *tile == crate::map::Tile::Door;
        let hovered = *pos == hovered_tile;
        let door_open = door && map.door_state(*pos).is_some_and(|d| d.open >= 1.0);
        let mix = atmo.mixture_at(*pos);
        let total = mix.total();
        let temp = thermal.amb[atmo.idx(*pos)];
        let p = crate::atmosphere::pressure(total, temp);
        // Hazard encoding: 0 none, 1 low O2, 2 high CO2, 3 pollutant.
        let hazard = if crate::atmosphere::partial_pressure(mix.mol[3], total, temp)
            > crate::atmosphere::POLLUTANT_HIGH_KPA
        {
            3
        } else if crate::atmosphere::partial_pressure(mix.mol[2], total, temp)
            > crate::atmosphere::CO2_HIGH_KPA
        {
            2
        } else if crate::atmosphere::partial_pressure(mix.mol[0], total, temp)
            < crate::atmosphere::O2_SAFE_KPA
            && total > 0.01
        {
            1
        } else {
            0
        };
        // 0.5 kPa quantization keeps buckets stable between sim events.
        let p_bucket = (p * 2.0).clamp(0.0, u32::MAX as f32) as u32;
        let bucket = (p_bucket & 0x00FF_FFFF)
            | (hazard << 24)
            | (u32::from(hovered) << 26)
            | (u32::from(door) << 27)
            | (u32::from(door_open) << 28);
        if bucket != vis.bucket[ti] {
            vis.bucket[ti] = bucket;
            let color = if door {
                door_overlay_color(door_open, hovered)
            } else {
                let base = match hazard {
                    3 => Color::srgb(0.85, 0.30, 0.95),
                    2 => Color::srgb(0.95, 0.55, 0.20),
                    1 => Color::srgb(0.45, 0.55, 0.75),
                    _ => crate::atmosphere::pressure_color(p),
                };
                if hovered {
                    brighten(base, 0.45)
                } else {
                    base
                }
            };
            if let Ok(mut sprite) = tile_q.get_mut(vis.tiles[ti]) {
                sprite.color = color;
                vis.color_writes += 1;
            }
        }
    }
}

/// Lift a color toward white by `f` (hover feedback).
fn brighten(c: Color, f: f32) -> Color {
    let Color::Srgba(v) = c else { return c };
    Color::Srgba(Srgba {
        red: v.red + (1.0 - v.red) * f,
        green: v.green + (1.0 - v.green) * f,
        blue: v.blue + (1.0 - v.blue) * f,
        alpha: v.alpha,
    })
}

/// Ventilation overlay (Slice 6): one pooled sprite per duct cell tinted by
/// derived duct pressure (reuse of the atmosphere pressure ramp), plus a
/// small flow arrow on cells with current flow, blower direction markers
/// (red = unpowered), vent mode dots and tank fill rings. Refresh is a
/// bucket compare on a wall-clock cadence; membership rebuilds only on duct
/// or device-set changes.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn ventilation_overlay_system(
    mut commands: Commands,
    real: Res<Time<Real>>,
    map: Res<ShipMap>,
    art: Res<Art>,
    ducts: Res<crate::ventilation::DuctGrid>,
    overlay: Res<crate::OverlayMode>,
    vents: Query<(&TilePos, &crate::ventilation::Vent)>,
    blowers: Query<(
        &TilePos,
        &crate::ventilation::Blower,
        &crate::power::PowerStatus,
    )>,
    tanks: Query<(&TilePos, &crate::ventilation::GasTank)>,
    mut vis: ResMut<VentOverlayVis>,
    mut root_q: Query<&mut Visibility, With<VentOverlayRoot>>,
    children_q: Query<&Children>,
    mut tile_q: Query<&mut Sprite, With<DuctTile>>,
    mut flow_q: Query<(&mut Sprite, &mut Transform), (With<DuctFlow>, Without<DuctTile>)>,
) {
    let active = *overlay == crate::OverlayMode::Ventilation;
    if let Ok(mut v) = root_q.get_mut(vis.root) {
        let want = if active {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *v != want {
            *v = want;
        }
    }
    if !active {
        return;
    }

    // Membership: duct version + device counts.
    let device_sig = (vents.iter().count() as u64)
        .wrapping_mul(1_000_003)
        .wrapping_add(blowers.iter().count() as u64)
        .wrapping_mul(1_000_033)
        .wrapping_add(tanks.iter().count() as u64);
    let sig = ducts.version ^ device_sig.rotate_left(17);
    if sig != vis.last_sig {
        vis.last_sig = sig;
        vis.rebuilds += 1;
        if let Ok(children) = children_q.get(vis.root) {
            for &c in children {
                commands.entity(c).despawn();
            }
        }
        vis.tiles.clear();
        vis.arrows.clear();
        vis.bucket.clear();
        for pos in ducts.iter_ducts() {
            let tile = commands
                .spawn((
                    DuctTile,
                    Sprite {
                        image: art.floor.clone(),
                        custom_size: Some(Vec2::splat(crate::TILE * 0.55)),
                        color: Color::WHITE,
                        ..default()
                    },
                    Transform::from_translation(map.world_pos(pos).extend(0.72)),
                ))
                .insert(ChildOf(vis.root))
                .id();
            let arrow = commands
                .spawn((
                    DuctFlow,
                    Sprite {
                        image: art.dot.clone(),
                        custom_size: Some(Vec2::new(crate::TILE * 0.34, crate::TILE * 0.16)),
                        color: Color::srgba(0.2, 1.0, 1.0, 1.0),
                        ..default()
                    },
                    Transform::from_translation(map.world_pos(pos).extend(0.73)),
                ))
                .insert(ChildOf(vis.root))
                .id();
            vis.tiles.push(tile);
            vis.arrows.push(arrow);
            vis.bucket.push(u32::MAX);
        }
    }

    vis.refresh_acc += real.delta_secs();
    if vis.refresh_acc < OVERLAY_REFRESH_SECS {
        return;
    }
    vis.refresh_acc = 0.0;

    let duct_list: Vec<TilePos> = ducts.iter_ducts().collect();
    for (ti, &pos) in duct_list.iter().enumerate() {
        let i = ducts.idx(pos);
        let p = ducts.pressure_at(pos);
        // 0.5 kPa quantization.
        let p_bucket = (p * 2.0).clamp(0.0, u32::MAX as f32) as u32;
        if p_bucket != vis.bucket[ti] {
            vis.bucket[ti] = p_bucket;
            if let Ok(mut sprite) = tile_q.get_mut(vis.tiles[ti]) {
                sprite.color = crate::atmosphere::pressure_color(p);
                vis.color_writes += 1;
            }
        }
        // Flow arrow: direction from telemetry, rotated to match.
        let (fx, fy) = (ducts.flow_x[i], ducts.flow_y[i]);
        let mag = (fx * fx + fy * fy).sqrt();
        if let Ok((mut sprite, mut tr)) = flow_q.get_mut(vis.arrows[ti]) {
            if mag > 0.05 {
                let angle = fy.atan2(fx);
                tr.rotation = Quat::from_rotation_z(angle);
                tr.translation = map.world_pos(pos).extend(0.73);
                sprite.color = Color::srgba(0.2, 1.0, 1.0, 1.0);
            } else {
                sprite.color = Color::srgba(0.0, 0.0, 0.0, 0.0);
            }
        }
    }
    // Blower markers: bright arrow ring, red when unpowered.
    for (pos, blower, power) in blowers.iter() {
        let Some(ti) = duct_list.iter().position(|&p| p == *pos) else {
            continue;
        };
        if let Ok(mut sprite) = tile_q.get_mut(vis.tiles[ti]) {
            sprite.color = if !blower.enabled {
                Color::srgb(0.45, 0.45, 0.5)
            } else if power.ok() {
                Color::srgb(0.35, 0.95, 1.0)
            } else {
                Color::srgb(1.0, 0.35, 0.3)
            };
            vis.color_writes += 1;
        }
        if let Ok((mut sprite, mut tr)) = flow_q.get_mut(vis.arrows[ti]) {
            let d = blower.dir.delta();
            let angle = (d.y as f32).atan2(d.x as f32);
            tr.rotation = Quat::from_rotation_z(angle);
            tr.translation = map.world_pos(*pos).extend(0.73);
            sprite.color = if blower.enabled && power.ok() {
                Color::srgba(0.4, 1.0, 1.0, 1.0)
            } else {
                Color::srgba(0.4, 0.4, 0.45, 0.6)
            };
        }
    }
    // Vent markers: mode color on the duct tile under them (dark = closed).
    for (pos, vent) in vents.iter() {
        let Some(ti) = duct_list.iter().position(|&p| p == *pos) else {
            continue;
        };
        if let Ok(mut sprite) = tile_q.get_mut(vis.tiles[ti]) {
            sprite.color = if !vent.open {
                Color::srgb(0.55, 0.25, 0.25)
            } else {
                match vent.mode {
                    crate::ventilation::VentMode::Supply => Color::srgb(0.4, 0.95, 0.5),
                    crate::ventilation::VentMode::Exhaust => Color::srgb(0.98, 0.7, 0.25),
                    crate::ventilation::VentMode::Balanced => Color::srgb(0.45, 0.85, 0.95),
                }
            };
            vis.color_writes += 1;
        }
    }
    // Tank markers: fill tint on the duct tile under them.
    for (pos, tank) in tanks.iter() {
        let Some(ti) = duct_list.iter().position(|&p| p == *pos) else {
            continue;
        };
        if let Ok(mut sprite) = tile_q.get_mut(vis.tiles[ti]) {
            let fill = (tank.total() / crate::ventilation::TANK_MOL).clamp(0.0, 1.0);
            sprite.color = Color::srgb(0.5 + fill * 0.4, 0.7 - fill * 0.3, 0.4);
            vis.color_writes += 1;
        }
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
    lang: Res<Lang>,
    cables: Res<CableGrid>,
    pipes: Res<crate::coolant::PipeGrid>,
    ducts: Res<crate::ventilation::DuctGrid>,
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
    let l = strings(*lang);
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
            let check = building::can_place(
                &map,
                kind,
                tile,
                &ground,
                &feet,
                |p| cables.has(p),
                |p| pipes.has(p),
                |p| ducts.has(p),
            );
            let (color, text) = match &check {
                Ok(()) => {
                    let cost: u32 = d.cost.iter().sum();
                    let name = if kind == BuildingKind::Door {
                        // Show the inferred passage axis on the ghost.
                        match crate::airtight::door_axis(&map, tile) {
                            Some(axis) => crate::tfmt!(l.fmt_lbl_door_axis, axis = axis.label()),
                            None => l.lbl_door.to_string(),
                        }
                    } else {
                        loc::building_label(kind, l).to_string()
                    };
                    (
                        Color::srgba(0.4, 1.0, 0.5, 0.55),
                        crate::tfmt!(l.fmt_lbl_place, name = name, cost = cost),
                    )
                }
                Err(e) => (
                    Color::srgba(1.0, 0.35, 0.3, 0.55),
                    loc::placement_error_label(e, l).to_string(),
                ),
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
                text2d.0 = l.lbl_deconstruct.to_string();
                tf.translation = (p + Vec2::new(0.0, 14.0)).extend(0.82);
                *vis = Visibility::Visible;
            }
        }
    }
}

/// Despawn visuals whose target entity no longer exists.
fn cleanup_visuals_system(
    mut index: ResMut<VisualIndex>,
    targets: Query<Entity, With<TilePos>>,
    visuals: Query<(Entity, &Visual)>,
    mut commands: Commands,
) {
    for (e, v) in visuals.iter() {
        if targets.get(v.target).is_err() {
            index.remove(v.target, v.role);
            commands.entity(e).despawn();
        }
    }
}
