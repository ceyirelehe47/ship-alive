//! Airtight compartments & doors (Slice 4).
//!
//! Two deliberately separate concepts:
//!
//! * **Structural compartments** — the partition of the ship interior by
//!   hull/wall geometry. Door tiles are *portals* (boundaries), not room
//!   volume. Purely derived cache data: rebuilt only when `ShipMap::version`
//!   changes (wall built/torn, door built/torn), never because a door opens.
//! * **Current airtight connectivity** — which compartments exchange air
//!   right now: two regions are connected when an *open* door portal joins
//!   them. A tiny union-find over the region/portal graph, recomputed only
//!   when a door's seal actually flips.
//!
//! Door runtime state lives on ECS entities (`Door`) and is mirrored into
//! `ShipMap`'s dense `DoorTileState` grid each change, so pathfinding and
//! movement read it with zero queries. Heat integration: a door tile stays an
//! ordinary air node with constant capacity (toggling can never create or
//! destroy heat) — only its *conductivity* to the neighbours changes, from
//! open-air fast exchange to a slow structure-like seep (see `thermal.rs`).

use crate::map::{ShipMap, Tile, TilePos};
use bevy::prelude::*;
use std::collections::{HashMap, HashSet};

// =====================================================================================
// Tunables (sim seconds; 60 sim s per real s at 1×)
// =====================================================================================

/// Full open / full close travel of the door leaf (0.4 real s at 1×).
pub const DOOR_MOVE_SECS: f32 = 24.0;
/// How long an auto door stays open after the last passage demand
/// (0.6 real s at 1×) — lets a stream of crew through without flapping.
pub const DOOR_HOLD_SECS: f64 = 36.0;

// =====================================================================================
// Door model
// =====================================================================================

/// Player-facing intent for one door.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DoorMode {
    /// Normal automatic door: closed when idle, opens for passage.
    #[default]
    Auto,
    /// Held open permanently (high-throughput corridors, future venting).
    HoldOpen,
    /// Locked shut: unwalkable, airtight, no crew may plan through it.
    LockClosed,
}

impl DoorMode {
    pub fn label(self) -> &'static str {
        match self {
            DoorMode::Auto => "Auto",
            DoorMode::HoldOpen => "Hold Open",
            DoorMode::LockClosed => "Lock Closed",
        }
    }

    pub const ALL: [DoorMode; 3] = [DoorMode::Auto, DoorMode::HoldOpen, DoorMode::LockClosed];
}

/// Which way the doorway passes. `Ns` = flanked by walls east+west, crew walk
/// north↔south through it; `Ew` = flanked north+south, crew walk east↔west.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DoorAxis {
    Ns,
    Ew,
}

impl DoorAxis {
    pub fn label(self) -> &'static str {
        match self {
            DoorAxis::Ns => "N-S",
            DoorAxis::Ew => "E-W",
        }
    }
}

/// Physical state of the leaf.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DoorPhase {
    #[default]
    Closed,
    Opening,
    Open,
    Closing,
}

impl DoorPhase {
    pub fn label(self) -> &'static str {
        match self {
            DoorPhase::Closed => "Closed",
            DoorPhase::Opening => "Opening",
            DoorPhase::Open => "Open",
            DoorPhase::Closing => "Closing",
        }
    }
}

/// Runtime state of one door device (a `Building { kind: Door }` entity).
#[derive(Component, Debug)]
pub struct Door {
    pub mode: DoorMode,
    pub phase: DoorPhase,
    /// 0 = closed .. 1 = fully open.
    pub progress: f32,
    /// Sim time until which an auto door keeps itself open after a demand.
    pub hold_until: f64,
    pub axis: DoorAxis,
    /// Completed open→close cycles (telemetry for the anti-flap guarantee).
    pub cycles: u32,
}

impl Door {
    pub fn new(axis: DoorAxis) -> Self {
        Self {
            mode: DoorMode::Auto,
            phase: DoorPhase::Closed,
            progress: 0.0,
            hold_until: 0.0,
            axis,
            cycles: 0,
        }
    }

    /// Crew may step onto the door tile only when the leaf is fully open.
    pub fn passable(&self) -> bool {
        self.progress >= 1.0
    }

    /// Airtight boundary while not fully open (Opening/Closing stay sealed —
    /// simple, deterministic rule).
    pub fn sealed(&self) -> bool {
        self.progress < 1.0
    }
}

/// Passage demands registered by the movement system this step (tile → the
/// door on it should be open). Consumed and cleared by `door_system`.
#[derive(Resource, Default, Debug)]
pub struct DoorDemand(pub HashSet<TilePos>);

// =====================================================================================
// Placement: orientation inference
// =====================================================================================

/// Infer a door's passage axis from its surroundings: the two lateral
/// neighbours must be wall (hull or built) and the two passage neighbours
/// must be open interior. Returns `None` for ambiguous or open-hall spots —
/// a door must sit in a real wall opening to be an airtight boundary.
pub fn door_axis(map: &ShipMap, p: TilePos) -> Option<DoorAxis> {
    let wall = |q: TilePos| !map.is_standable(q); // OOB / walls / machines block
    let open = |q: TilePos| map.is_standable(q);
    let n = TilePos::new(p.x, p.y - 1);
    let s = TilePos::new(p.x, p.y + 1);
    let e = TilePos::new(p.x + 1, p.y);
    let w = TilePos::new(p.x - 1, p.y);
    // Ns: walls flank east+west, passage north-south.
    if wall(e) && wall(w) && open(n) && open(s) {
        Some(DoorAxis::Ns)
    } else if wall(n) && wall(s) && open(e) && open(w) {
        Some(DoorAxis::Ew)
    } else {
        None
    }
}

// =====================================================================================
// Structural compartments (derived cache)
// =====================================================================================

/// Region id meaning "not interior air volume" (walls, doors, OOB).
pub const NO_REGION: u16 = u16::MAX;

/// Summary of one structural compartment.
#[derive(Clone, Debug)]
pub struct RegionInfo {
    pub id: u16,
    pub cells: u32,
    pub centroid: TilePos,
    pub bounding_min: TilePos,
    pub bounding_max: TilePos,
    /// Some interior tile touches the map exterior (out of bounds): the
    /// region would vent to space. Identification only this slice.
    pub exposed: bool,
}

/// One door as a topology edge between two regions.
#[derive(Clone, Copy, Debug)]
pub struct PortalDoor {
    pub entity: Option<Entity>,
    pub pos: TilePos,
    pub axis: DoorAxis,
    /// Region on the passage's "from" side (north for Ns, west for Ew).
    pub side_a: u16,
    /// Region on the "to" side (south / east). Either side may be NO_REGION
    /// when the door abuts structure instead of air.
    pub side_b: u16,
}

/// Derived compartment topology + current airtight connectivity. Not
/// authoritative: throw this away and rebuild at any time.
#[derive(Resource, Debug, Clone)]
pub struct Compartments {
    /// Dense per-tile region id (NO_REGION for walls/doors/OOB).
    pub id: Vec<u16>,
    pub regions: Vec<RegionInfo>,
    pub doors: Vec<PortalDoor>,
    /// Air-connectivity group per region id: regions joined by open doors
    /// share a group. Recomputed only when a door seal flips or on rebuild.
    pub air_group: Vec<u16>,
    pub air_groups: u16,
    width: i32,
    height: i32,
    /// `ShipMap::version` this partition was built from.
    pub geometry_version: u64,
    /// Telemetry: structural rebuilds / air recomputes since resource birth.
    pub rebuilds: u32,
    pub air_recomputes: u32,
    /// Door tallies refreshed by `door_system` every step (UI summary only).
    pub doors_open: u32,
    pub doors_closed: u32,
}

/// Tiles that carry room air volume (machine footprints included — their
/// device heat mass couples to the same node per the Slice 3 model).
fn air_tile(t: Option<Tile>) -> bool {
    matches!(t, Some(Tile::Floor) | Some(Tile::Machine))
}

impl Compartments {
    /// Flood-fill the structural partition from the map geometry.
    pub fn rebuild(map: &ShipMap) -> Self {
        let n = (map.width * map.height) as usize;
        let mut id = vec![NO_REGION; n];
        let mut regions: Vec<RegionInfo> = Vec::new();
        for y in 0..map.height {
            for x in 0..map.width {
                let p = TilePos::new(x, y);
                if !air_tile(map.tile(p)) || id[(y * map.width + x) as usize] != NO_REGION {
                    continue;
                }
                let rid = regions.len() as u16;
                let mut cells: Vec<TilePos> = Vec::new();
                let mut stack = vec![p];
                id[(y * map.width + x) as usize] = rid;
                while let Some(c) = stack.pop() {
                    cells.push(c);
                    for nb in [
                        TilePos::new(c.x + 1, c.y),
                        TilePos::new(c.x - 1, c.y),
                        TilePos::new(c.x, c.y + 1),
                        TilePos::new(c.x, c.y - 1),
                    ] {
                        if !map.in_bounds(nb) {
                            continue;
                        }
                        let j = (nb.y * map.width + nb.x) as usize;
                        if air_tile(map.tile(nb)) && id[j] == NO_REGION {
                            id[j] = rid;
                            stack.push(nb);
                        }
                    }
                }
                let count = cells.len();
                let (sx, sy) = cells.iter().fold((0i64, 0i64), |(ax, ay), c| {
                    (ax + c.x as i64, ay + c.y as i64)
                });
                let centroid = TilePos::new((sx / count as i64) as i32, (sy / count as i64) as i32);
                let mut min = cells[0];
                let mut max = cells[0];
                let mut exposed = false;
                for &c in &cells {
                    min.x = min.x.min(c.x);
                    min.y = min.y.min(c.y);
                    max.x = max.x.max(c.x);
                    max.y = max.y.max(c.y);
                    // Exterior contact: a 4-adjacent out-of-bounds tile.
                    if [
                        TilePos::new(c.x + 1, c.y),
                        TilePos::new(c.x - 1, c.y),
                        TilePos::new(c.x, c.y + 1),
                        TilePos::new(c.x, c.y - 1),
                    ]
                    .iter()
                    .any(|&nb| !map.in_bounds(nb))
                    {
                        exposed = true;
                    }
                }
                regions.push(RegionInfo {
                    id: rid,
                    cells: count as u32,
                    centroid,
                    bounding_min: min,
                    bounding_max: max,
                    exposed,
                });
            }
        }
        // Doors → portal edges. The axis inference is a property of the
        // surrounding geometry, valid at rebuild time by the placement rule.
        let mut doors = Vec::new();
        for (p, tile) in map.iter_tiles() {
            if tile != Tile::Door {
                continue;
            }
            let Some(axis) = door_axis(map, p) else {
                continue; // geometry drifted (e.g. adjacent door); no portal
            };
            let (a, b) = match axis {
                DoorAxis::Ns => (TilePos::new(p.x, p.y - 1), TilePos::new(p.x, p.y + 1)),
                DoorAxis::Ew => (TilePos::new(p.x - 1, p.y), TilePos::new(p.x + 1, p.y)),
            };
            let side = |q: TilePos| {
                if map.in_bounds(q) {
                    id[(q.y * map.width + q.x) as usize]
                } else {
                    NO_REGION
                }
            };
            doors.push(PortalDoor {
                entity: None,
                pos: p,
                axis,
                side_a: side(a),
                side_b: side(b),
            });
        }
        let mut me = Self {
            id,
            regions,
            doors,
            air_group: Vec::new(),
            air_groups: 0,
            width: map.width,
            height: map.height,
            geometry_version: map.version,
            rebuilds: 1,
            air_recomputes: 0,
            doors_open: 0,
            doors_closed: 0,
        };
        me.recompute_air(map);
        me
    }

    /// Region id at a tile (NO_REGION for walls, doors and out of bounds).
    pub fn region_at(&self, p: TilePos) -> u16 {
        if p.x < 0 || p.y < 0 || p.x >= self.width || p.y >= self.height {
            return NO_REGION;
        }
        self.id[(p.y * self.width + p.x) as usize]
    }

    /// Recompute current airtight connectivity: union regions across portals
    /// whose door is currently unsealed (fully open). Cost is proportional to
    /// the region/portal graph — never the tile count.
    pub fn recompute_air(&mut self, map: &ShipMap) {
        self.air_recomputes += 1;
        let n = self.regions.len();
        let mut parent: Vec<u16> = (0..n as u16).collect();
        let find = |parent: &mut Vec<u16>, mut x: u16| -> u16 {
            while parent[x as usize] != x {
                parent[x as usize] = parent[parent[x as usize] as usize];
                x = parent[x as usize];
            }
            x
        };
        for portal in &self.doors {
            let open = map.door_state(portal.pos).is_some_and(|d| d.open >= 1.0);
            if !open {
                continue;
            }
            if portal.side_a != NO_REGION && portal.side_b != NO_REGION {
                let ra = find(&mut parent, portal.side_a);
                let rb = find(&mut parent, portal.side_b);
                if ra != rb {
                    parent[ra.max(rb) as usize] = ra.min(rb);
                }
            }
        }
        // Normalize: group index = smallest member region id, compacted.
        let mut remap: HashMap<u16, u16> = HashMap::new();
        let mut air_group = vec![NO_REGION; n];
        for r in 0..n as u16 {
            let root = find(&mut parent, r);
            let next = remap.len() as u16;
            let g = *remap.entry(root).or_insert(next);
            air_group[r as usize] = g;
        }
        self.air_group = air_group;
        self.air_groups = remap.len() as u16;
    }

    /// Number of sealed (not exposed) regions.
    pub fn sealed_count(&self) -> u32 {
        self.regions.iter().filter(|r| !r.exposed).count() as u32
    }

    pub fn exposed_count(&self) -> u32 {
        self.regions.iter().filter(|r| r.exposed).count() as u32
    }
}

// =====================================================================================
// Unified environmental boundary (the Atmosphere-facing API)
// =====================================================================================

/// May the environment (heat today; gas, smoke, pressure tomorrow) exchange
/// *directly* between two orthogonally adjacent tiles? Structure still leaks
/// slowly in the thermal model — that is conduction, not direct exchange,
/// and will become a permeability coefficient later.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Boundary {
    Blocked,
    Open,
}

pub fn boundary(map: &ShipMap, a: TilePos, b: TilePos) -> Boundary {
    let ax = a.x - b.x;
    let ay = a.y - b.y;
    if ax.abs() + ay.abs() != 1 {
        return Boundary::Blocked; // not orthogonally adjacent
    }
    let side = |p: TilePos| -> Boundary {
        match map.tile(p) {
            Some(Tile::Door) => {
                if map.door_state(p).is_some_and(|d| d.open >= 1.0) {
                    Boundary::Open
                } else {
                    Boundary::Blocked
                }
            }
            Some(Tile::Floor) | Some(Tile::Machine) => Boundary::Open,
            _ => Boundary::Blocked,
        }
    };
    match (side(a), side(b)) {
        (Boundary::Open, Boundary::Open) => Boundary::Open,
        _ => Boundary::Blocked,
    }
}

// =====================================================================================
// Systems
// =====================================================================================

pub struct AirtightPlugin;

impl Plugin for AirtightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DoorDemand>();
        app.add_systems(
            FixedUpdate,
            // After the job systems (freshly built doors are seen this step;
            // demolished doors are already gone from the grid — their entity
            // despawn is deferred, so door_system stays defensive about
            // tiles), before movement consumes the door states.
            door_system
                .after(crate::jobs::crew_scan_system)
                .before(crate::Set::Move)
                .in_set(crate::Set::Jobs),
        );
        app.add_systems(
            Update,
            (compartment_sync_system, door_action_system).in_set(crate::Set::Sync),
        );
    }
}

/// Rebuild the structural partition when (and only when) the geometry
/// changed, then match door entities onto portals. Never touches anything
/// while the world is stable — one integer compare per frame.
pub fn compartment_sync_system(
    map: Res<ShipMap>,
    mut comps: ResMut<Compartments>,
    doors: Query<(Entity, &TilePos, &Door)>,
) {
    if comps.geometry_version == map.version {
        return;
    }
    let mut next = Compartments::rebuild(&map);
    next.rebuilds = comps.rebuilds + 1;
    let by_pos: HashMap<TilePos, Entity> = doors.iter().map(|(e, p, _)| (*p, e)).collect();
    for portal in &mut next.doors {
        portal.entity = by_pos.get(&portal.pos).copied();
    }
    *comps = next;
}

/// Advance every door in sim time: consume passage demands, keep the leaf
/// open while the doorway is occupied, never close onto a crew member, and
/// mirror state into the map (pathfinding) + thermal seal (heat) + air
/// connectivity graph when a seal actually flips.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
pub fn door_system(
    clock: Res<crate::simtime::SimClock>,
    mut map: ResMut<ShipMap>,
    mut thermal: ResMut<crate::thermal::ThermalGrid>,
    // Optional so minimal test worlds can run the door logic standalone;
    // the full app always inserts the grid at setup.
    mut atmo: Option<ResMut<crate::atmosphere::AtmosphereGrid>>,
    mut comps: ResMut<Compartments>,
    mut demand: ResMut<DoorDemand>,
    mut doors: Query<(Entity, &TilePos, &mut Door)>,
    crews: Query<(&TilePos, &crate::crew::Movement), With<crate::crew::Crew>>,
) {
    let dt = clock.dt() as f32;
    let now = clock.now();
    // Doorway occupancy: crew standing on the tile, or about to step onto it.
    let mut occupied: HashSet<TilePos> = HashSet::new();
    for (pos, mov) in crews.iter() {
        occupied.insert(*pos);
        if let Some(&next) = mov.path.first() {
            occupied.insert(next);
        }
    }
    let mut any_seal_flip = false;
    let mut open_count = 0u32;
    let mut closed_count = 0u32;
    for (_e, pos, mut door) in doors.iter_mut() {
        let pos = *pos;
        // Demolished this step: the tile already flipped back to floor while
        // this entity's despawn is still in flight. Nothing left to drive.
        if map.tile(pos) != Some(Tile::Door) {
            continue;
        }
        let held = occupied.contains(&pos) || demand.0.contains(&pos);
        // Target leaf position per mode. Auto extends its hold window on any
        // demand or occupancy so a stream of crew passes without flapping.
        let target_open = match door.mode {
            DoorMode::HoldOpen => true,
            DoorMode::LockClosed => false,
            DoorMode::Auto => {
                if held {
                    door.hold_until = now + DOOR_HOLD_SECS;
                }
                now < door.hold_until
            }
        };
        // Never close onto a crew member: the leaf freezes (does not reverse)
        // while the doorway is occupied.
        let closing_blocked = !target_open && occupied.contains(&pos);
        if target_open && door.progress < 1.0 {
            door.progress = (door.progress + dt / DOOR_MOVE_SECS).min(1.0);
            door.phase = if door.progress >= 1.0 {
                DoorPhase::Open
            } else {
                DoorPhase::Opening
            };
        } else if !target_open && door.progress > 0.0 && !closing_blocked {
            door.progress = (door.progress - dt / DOOR_MOVE_SECS).max(0.0);
            door.phase = if door.progress <= 0.0 {
                door.cycles = door.cycles.wrapping_add(1);
                DoorPhase::Closed
            } else {
                DoorPhase::Closing
            };
        } else if door.progress >= 1.0 {
            door.phase = DoorPhase::Open;
        } else {
            door.phase = DoorPhase::Closed;
        }
        // Mirror runtime state for pathfinding/movement. Always written (a
        // plain struct copy): the lock flag flips on mode changes even when
        // the leaf itself never moved.
        map.set_door_state(
            pos,
            crate::map::DoorTileState {
                open: door.progress,
                locked: door.mode == DoorMode::LockClosed,
            },
        );
        // Thermal seal + connectivity only on real flips (never per step).
        if door.sealed() != thermal.door_sealed_at(pos) {
            thermal.set_door_sealed(pos, door.sealed());
            // Gas starts (or stops) exchanging across this doorway this very
            // step: wake the door cell and both sides — the deterministic
            // door/atmosphere sync contract.
            if let Some(a) = &mut atmo {
                a.wake_around(pos);
            }
            any_seal_flip = true;
        }
        if door.progress >= 1.0 {
            open_count += 1;
        } else {
            closed_count += 1;
        }
    }
    if any_seal_flip {
        comps.recompute_air(&map);
    }
    comps.doors_open = open_count;
    comps.doors_closed = closed_count;
    demand.0.clear();
}

/// Player action: set a door's mode (Auto / Hold Open / Lock Closed). Runs on
/// the frame-based Update schedule next to the other action consumers.
pub fn door_action_system(
    mut events: EventReader<crate::jobs::Action>,
    mut doors: Query<(Entity, &TilePos, &mut Door)>,
    mut log: ResMut<crate::log::EventLog>,
    clock: Res<crate::simtime::SimClock>,
) {
    let now = clock.now();
    for action in events.read() {
        if let crate::jobs::Action::SetDoorMode { door, mode } = *action {
            if let Ok((_, pos, mut d)) = doors.get_mut(door) {
                d.mode = mode;
                log.push(
                    now,
                    crate::log::LogKind::Info,
                    format!("Door at ({},{}) -> {}", pos.x, pos.y, mode.label()),
                );
            }
        }
    }
}
