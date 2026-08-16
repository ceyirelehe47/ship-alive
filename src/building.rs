//! Player-built interior structures: definitions, blueprints, construction
//! and deconstruction.
//!
//! Buildings are ECS entities layered over the dense `ShipMap` grid. While a
//! building is still a blueprint its tiles stay walkable floor; on completion
//! the grid tiles flip to the kind's tile type (`BuiltWall` / `Door` /
//! `Machine`) so pathfinding reacts immediately. Deconstruction restores
//! floor tiles and refunds the full build cost (provisional Slice 1 design
//! to encourage layout experiments).

use crate::items::{self, ItemKind};
use crate::map::{ShipMap, Tile, TilePos};
use bevy::prelude::*;

/// Everything buildable.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum BuildingKind {
    Wall,
    Door,
    Rack,
    Fabricator,
    /// Underfloor power cable (1 tile; lives in the CableGrid, not as a
    /// resting ECS entity).
    PowerCable,
    /// Starter Reactor: a 2x2 generator device.
    Reactor,
}

impl BuildingKind {
    pub const ALL: [BuildingKind; 6] = [
        BuildingKind::Wall,
        BuildingKind::Door,
        BuildingKind::Rack,
        BuildingKind::Fabricator,
        BuildingKind::PowerCable,
        BuildingKind::Reactor,
    ];

    pub fn label(&self) -> &'static str {
        def(*self).label
    }
}

/// Static definition of one buildable kind.
pub struct BuildingDef {
    pub kind: BuildingKind,
    pub label: &'static str,
    pub w: i32,
    pub h: i32,
    /// Build cost indexed by `ItemKind::index` ([crate, ore, part]).
    pub cost: [u32; 3],
    /// Crew construction time in sim seconds (60 sim s per real s at 1×).
    pub work_secs: f32,
    /// Crew deconstruction time in sim seconds.
    pub demo_secs: f32,
    /// Whether the finished building blocks movement.
    pub blocks: bool,
    /// Whether ground items on the footprint block placement.
    pub needs_clear_tiles: bool,
    /// Tile written into the grid on completion (floor kinds keep `Floor`).
    pub tile: Tile,
}

pub fn def(kind: BuildingKind) -> BuildingDef {
    match kind {
        BuildingKind::Wall => BuildingDef {
            kind,
            label: "Wall",
            w: 1,
            h: 1,
            cost: [0, 0, 1],
            work_secs: 180.0,
            demo_secs: 90.0,
            blocks: true,
            needs_clear_tiles: true,
            tile: Tile::BuiltWall,
        },
        BuildingKind::Door => BuildingDef {
            kind,
            label: "Door",
            w: 1,
            h: 1,
            cost: [0, 0, 2],
            work_secs: 180.0,
            demo_secs: 90.0,
            blocks: false,
            needs_clear_tiles: false,
            tile: Tile::Door,
        },
        BuildingKind::Rack => BuildingDef {
            kind,
            label: "Storage Rack",
            w: 1,
            h: 1,
            cost: [0, 0, 1],
            work_secs: 150.0,
            demo_secs: 90.0,
            blocks: false,
            needs_clear_tiles: false,
            tile: Tile::Floor,
        },
        BuildingKind::Fabricator => BuildingDef {
            kind,
            label: "Fabricator",
            w: 2,
            h: 2,
            cost: [0, 0, 4],
            work_secs: 480.0,
            demo_secs: 180.0,
            blocks: true,
            needs_clear_tiles: true,
            tile: Tile::Machine,
        },
        BuildingKind::PowerCable => BuildingDef {
            kind,
            label: "Power Cable",
            w: 1,
            h: 1,
            cost: [0, 0, 0],
            work_secs: 90.0,
            demo_secs: 60.0,
            blocks: false,
            needs_clear_tiles: false,
            tile: Tile::Floor,
        },
        BuildingKind::Reactor => BuildingDef {
            kind,
            label: "Reactor",
            w: 2,
            h: 2,
            cost: [0, 0, 8],
            work_secs: 600.0,
            demo_secs: 240.0,
            blocks: true,
            needs_clear_tiles: true,
            tile: Tile::Machine,
        },
    }
}

/// Rectangular grid footprint. `x`/`y` is the top-left tile.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Footprint {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Footprint {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, p: TilePos) -> bool {
        p.x >= self.x && p.x < self.x + self.w && p.y >= self.y && p.y < self.y + self.h
    }

    pub fn tiles(&self) -> impl Iterator<Item = TilePos> + '_ {
        let (x0, y0, w, h) = (self.x, self.y, self.w, self.h);
        (0..h).flat_map(move |dy| (0..w).map(move |dx| TilePos::new(x0 + dx, y0 + dy)))
    }

    /// Manhattan distance from a position to the nearest footprint tile.
    pub fn distance_to(&self, p: TilePos) -> f32 {
        // Octile distance from `p` to the nearest footprint tile (8-way
        // geometry — Manhattan would systematically overestimate diagonal
        // targets in the job scan).
        self.tiles()
            .map(|t| crate::path::octile_distance(t, p))
            .fold(f32::INFINITY, f32::min)
    }
}

/// A placed-but-not-yet-built building. Materials are hauled into
/// `delivered`; once every cost slot is filled a builder can construct it.
#[derive(Component, Debug)]
pub struct Blueprint {
    pub kind: BuildingKind,
    pub foot: Footprint,
    /// Materials already on site, indexed by `ItemKind::index`.
    pub delivered: [u32; 3],
    /// 0..1 construction progress (driven by the building crew).
    pub progress: f32,
}

impl Blueprint {
    pub fn missing(&self, kind: ItemKind) -> u32 {
        let def = def(self.kind);
        def.cost[kind.index()].saturating_sub(self.delivered[kind.index()])
    }

    /// Materials still to be hauled, as (kind, count) pairs.
    pub fn missing_list(&self) -> Vec<(ItemKind, u32)> {
        let def = def(self.kind);
        ItemKind::ALL
            .iter()
            .filter_map(|&k| {
                let miss = def.cost[k.index()].saturating_sub(self.delivered[k.index()]);
                (miss > 0).then_some((k, miss))
            })
            .collect()
    }

    pub fn fully_supplied(&self) -> bool {
        self.missing_list().is_empty()
    }

    pub fn materials_label(&self) -> String {
        let def = def(self.kind);
        ItemKind::ALL
            .iter()
            .filter(|k| def.cost[k.index()] > 0)
            .map(|k| {
                format!(
                    "{} {}/{}",
                    short_label(*k),
                    self.delivered[k.index()],
                    def.cost[k.index()]
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn short_label(k: ItemKind) -> &'static str {
    match k {
        ItemKind::Crate => "crate",
        ItemKind::Ore => "ore",
        ItemKind::Part => "part",
    }
}

/// A completed building.
#[derive(Component, Debug)]
pub struct Building {
    pub kind: BuildingKind,
    pub foot: Footprint,
    /// 0..1 deconstruction progress (driven by the demolishing crew).
    pub demo_progress: f32,
}

/// Player intent: tear this building down (full material refund).
#[derive(Component)]
pub struct MarkedForDeconstruct;

// =====================================================================================
// Placement
// =====================================================================================

/// Why a placement is illegal — surfaced to the build ghost and event log.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlacementError {
    OutsideShip,
    NotFloor,
    ItemInWay,
    OverlapsBuilding,
    OverlapsBlueprint,
    CableExists,
}

impl PlacementError {
    pub fn label(&self) -> &'static str {
        match self {
            PlacementError::OutsideShip => "outside the ship hull",
            PlacementError::NotFloor => "not on clear floor",
            PlacementError::ItemInWay => "an item is in the way",
            PlacementError::OverlapsBuilding => "overlaps an existing building",
            PlacementError::OverlapsBlueprint => "overlaps another blueprint",
            PlacementError::CableExists => "a cable already runs here",
        }
    }
}

/// Check whether a footprint can host a new blueprint.
pub fn can_place(
    map: &ShipMap,
    kind: BuildingKind,
    origin: TilePos,
    ground_items: &[TilePos],
    buildings: &[(Footprint, bool)], // (footprint, is_blueprint)
    has_cable: impl Fn(TilePos) -> bool,
) -> Result<(), PlacementError> {
    let d = def(kind);
    let foot = Footprint::new(origin.x, origin.y, d.w, d.h);
    if kind == BuildingKind::PowerCable {
        // Underfloor: any interior tile works (under room walls, machines,
        // doors), but never the hull shell — i.e. never the map border —
        // never twice, never under a pending cable plan.
        let on_border = origin.x == 0
            || origin.y == 0
            || origin.x == map.width - 1
            || origin.y == map.height - 1;
        return if on_border || !map.in_bounds(origin) {
            Err(PlacementError::OutsideShip)
        } else {
            match map.tile(origin) {
                _ if has_cable(origin) => Err(PlacementError::CableExists),
                _ => {
                    let dup_bp = buildings
                        .iter()
                        .any(|(f, is_bp)| *is_bp && f.w == 1 && f.h == 1 && f.contains(origin));
                    if dup_bp {
                        Err(PlacementError::CableExists)
                    } else {
                        Ok(())
                    }
                }
            }
        };
    }
    for t in foot.tiles() {
        match map.tile(t) {
            Some(Tile::Floor) => {}
            Some(Tile::Door) | Some(Tile::Machine) | Some(Tile::BuiltWall) | Some(Tile::Wall) => {
                return Err(PlacementError::NotFloor)
            }
            None => return Err(PlacementError::OutsideShip),
        }
        if d.needs_clear_tiles && ground_items.contains(&t) {
            return Err(PlacementError::ItemInWay);
        }
    }
    for (other, is_bp) in buildings {
        if foot_overlap(&foot, other) {
            return Err(if *is_bp {
                PlacementError::OverlapsBlueprint
            } else {
                PlacementError::OverlapsBuilding
            });
        }
    }
    Ok(())
}

fn foot_overlap(a: &Footprint, b: &Footprint) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// Tiles a crew member can stand on to interact with a footprint: any
/// walkable footprint tile (blueprints are walkable) plus the 4-adjacent
/// perimeter.
pub fn interaction_tiles(map: &ShipMap, foot: &Footprint) -> Vec<TilePos> {
    let mut out: Vec<TilePos> = foot.tiles().filter(|&t| map.is_walkable(t)).collect();
    for t in foot.tiles() {
        for n in [
            TilePos::new(t.x + 1, t.y),
            TilePos::new(t.x - 1, t.y),
            TilePos::new(t.x, t.y + 1),
            TilePos::new(t.x, t.y - 1),
        ] {
            if map.is_walkable(n) && !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out
}

/// Path from `from` to the closest reachable interaction tile of a footprint.
pub fn path_to_interaction(map: &ShipMap, from: TilePos, foot: &Footprint) -> Option<Vec<TilePos>> {
    let mut tiles = interaction_tiles(map, foot);
    tiles.sort_by(|a, b| {
        crate::path::octile_distance(*a, from)
            .partial_cmp(&crate::path::octile_distance(*b, from))
            .unwrap()
    });
    for t in tiles {
        if let Some(p) = crate::path::find_path(map, from, t, |_| false) {
            return Some(p);
        }
    }
    None
}

// =====================================================================================
// Construction / deconstruction effects
// =====================================================================================

/// Spawn a blueprint entity at `origin` (top-left tile of the footprint).
pub fn spawn_blueprint(commands: &mut Commands, kind: BuildingKind, origin: TilePos) -> Entity {
    let d = def(kind);
    commands
        .spawn((
            TilePos::new(origin.x, origin.y),
            Footprint::new(origin.x, origin.y, d.w, d.h),
            Blueprint {
                kind,
                foot: Footprint::new(origin.x, origin.y, d.w, d.h),
                delivered: [0, 0, 0],
                progress: 0.0,
            },
        ))
        .id()
}

/// Finish construction: replace the blueprint with a real building, flip grid
/// tiles, shove any crew standing on newly blocked tiles aside (applied via
/// commands so the running crew query is untouched), and refund over-delivered
/// materials (claim races can overshoot by one).
#[allow(clippy::too_many_arguments)]
pub fn complete_building(
    commands: &mut Commands,
    map: &mut ShipMap,
    cables: &mut crate::power::CableGrid,
    blueprint: Entity,
    bp: &Blueprint,
    crew_positions: &[(Entity, TilePos)],
    ground_now: &[TilePos],
    log: &mut crate::log::EventLog,
    stats: &mut crate::stats::Stats,
    now: f64,
) {
    let d = def(bp.kind);
    if bp.kind == BuildingKind::PowerCable {
        // Cables rest in the dense underfloor grid, never as entities.
        for t in bp.foot.tiles() {
            cables.set(t, true);
        }
        commands.entity(blueprint).despawn();
        stats.built += 1;
        log.push(
            now,
            crate::log::LogKind::Job,
            format!("Power cable laid at ({},{})", bp.foot.x, bp.foot.y),
        );
        return;
    }
    for t in bp.foot.tiles() {
        map.set_tile(t, d.tile);
    }
    // Displace crews standing on tiles that just became blocked.
    if d.blocks {
        for &(e, pos) in crew_positions {
            if bp.foot.contains(pos) {
                if let Some(free) = nearest_walkable(map, pos) {
                    commands.entity(e).insert(TilePos::new(free.x, free.y));
                    commands.entity(e).insert(crate::crew::Movement::default());
                }
            }
        }
    }
    // Refund over-delivered materials (materials conservation).
    let mut occupied: Vec<TilePos> = ground_now.to_vec();
    for kind in ItemKind::ALL {
        let excess = bp.delivered[kind.index()].saturating_sub(d.cost[kind.index()]);
        for _ in 0..excess {
            if let Some(t) =
                crate::map::find_drop_tile_ext(map, TilePos::new(bp.foot.x, bp.foot.y), &occupied)
            {
                occupied.push(t);
                items::spawn_item(commands, t, kind);
            }
        }
    }
    commands.entity(blueprint).despawn();
    let mut ec = commands.spawn((
        TilePos::new(bp.foot.x, bp.foot.y),
        bp.foot,
        Building {
            kind: bp.kind,
            foot: bp.foot,
            demo_progress: 0.0,
        },
    ));
    match bp.kind {
        BuildingKind::Rack => {
            ec.insert(crate::storage::StorageCell::default());
        }
        BuildingKind::Fabricator => {
            ec.insert(crate::production::Fabricator::default());
            ec.insert(crate::power::PowerRole::consumer(
                crate::power::FABRICATOR_DEMAND,
            ));
            ec.insert(crate::power::PowerStatus::default());
        }
        BuildingKind::Reactor => {
            ec.insert(crate::power::PowerRole::generator());
            ec.insert(crate::power::PowerStatus::default());
        }
        _ => {}
    }
    stats.built += 1;
    log.push(
        now,
        crate::log::LogKind::Job,
        format!("{} built at ({},{})", d.label, bp.foot.x, bp.foot.y),
    );
}

/// Nearest walkable tile to `around` (excluding `around` itself), searching
/// outward in growing square rings.
pub fn nearest_walkable(map: &ShipMap, around: TilePos) -> Option<TilePos> {
    for r in 1i32..6 {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx.abs() != r && dy.abs() != r {
                    continue; // ring border only
                }
                let p = TilePos::new(around.x + dx, around.y + dy);
                if map.is_walkable(p) {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Finish deconstruction: restore floor tiles, drop the full cost refund (and
/// any rack contents) as ground items, remove the building.
#[allow(clippy::too_many_arguments)]
pub fn complete_deconstruction(
    commands: &mut Commands,
    map: &mut ShipMap,
    cables: &mut crate::power::CableGrid,
    building: Entity,
    b: &Building,
    rack_contents: Option<[u32; 3]>,
    ground_now: &[TilePos],
    log: &mut crate::log::EventLog,
    stats: &mut crate::stats::Stats,
    now: f64,
) {
    let d = def(b.kind);
    if b.kind == BuildingKind::PowerCable {
        // The transient tile entity goes away; the cable leaves the grid.
        for t in b.foot.tiles() {
            cables.set(t, false);
        }
        commands.entity(building).despawn();
        stats.deconstructed += 1;
        log.push(
            now,
            crate::log::LogKind::Job,
            format!("Power cable removed at ({},{})", b.foot.x, b.foot.y),
        );
        return;
    }
    for t in b.foot.tiles() {
        map.set_tile(t, Tile::Floor);
    }
    // Full refund of the build cost, plus everything a rack had stored.
    let mut occupied: Vec<TilePos> = ground_now.to_vec();
    let mut drops: Vec<(TilePos, ItemKind)> = Vec::new();
    let mut want_drop = |kind: ItemKind| {
        if let Some(t) =
            crate::map::find_drop_tile_ext(map, TilePos::new(b.foot.x, b.foot.y), &occupied)
        {
            occupied.push(t);
            drops.push((t, kind));
        }
    };
    for (kind, n) in ItemKind::ALL.iter().zip(d.cost.iter()) {
        for _ in 0..*n {
            want_drop(*kind);
        }
    }
    if let Some(counts) = rack_contents {
        for kind in ItemKind::ALL {
            for _ in 0..counts[kind.index()] {
                want_drop(kind);
            }
        }
    }
    for (t, kind) in drops {
        items::spawn_item(commands, t, kind);
    }
    commands.entity(building).despawn();
    stats.deconstructed += 1;
    log.push(
        now,
        crate::log::LogKind::Job,
        format!("{} deconstructed — materials refunded", d.label),
    );
}
