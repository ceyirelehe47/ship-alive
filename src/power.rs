//! Ship power: an underfloor cable grid with real topology.
//!
//! Cables live in a dense grid layer parallel to `ShipMap` — they are *not*
//! ECS entities, never block ground movement and may share tiles with any
//! floor-side content. Devices (generators / consumers) are ECS entities that
//! attach to a cable network through their "power interface": any cable tile
//! on their footprint or directly adjacent to it.
//!
//! Networks are derived data: every frame the cable layer is flood-filled
//! into connected regions, devices attach, and per-network
//! generation/demand/served is computed. Consumers whose demand cannot be
//! served are shed in a deterministic order (entity id ≈ build order: older
//! devices keep power). Nothing about the topology is stored authoritatively
//! anywhere else, so runtime edits (place / cut / merge / toggle) can never
//! leave the model stale.

use crate::building::Footprint;
use crate::map::{ShipMap, TilePos};
use bevy::prelude::*;
use std::collections::HashMap;

/// Starter Reactor maximum output (abstract power units).
pub const REACTOR_OUTPUT: u32 = 100;
/// Fabricator draw while connected.
pub const FABRICATOR_DEMAND: u32 = 20;

/// Which side of the grid a device is on.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
pub enum PowerRole {
    Generator { output: u32, on: bool },
    Consumer { demand: u32 },
}

impl PowerRole {
    pub fn generator() -> Self {
        PowerRole::Generator {
            output: REACTOR_OUTPUT,
            on: true,
        }
    }

    pub fn consumer(demand: u32) -> Self {
        PowerRole::Consumer { demand }
    }
}

/// Current supply situation of one device (written by the power system).
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PowerStatus {
    /// No cable at the device's power interface.
    #[default]
    Unconnected,
    /// Connected, but the network has no online generator.
    NoGenerator,
    /// Connected, but the network cannot serve this device (overload shed).
    Shed,
    /// Connected and served.
    Powered,
}

impl PowerStatus {
    pub fn ok(self) -> bool {
        self == PowerStatus::Powered
    }

    pub fn label(self) -> &'static str {
        match self {
            PowerStatus::Unconnected => "no cable",
            PowerStatus::NoGenerator => "no generator",
            PowerStatus::Shed => "power shortage",
            PowerStatus::Powered => "powered",
        }
    }

    pub fn color(self) -> Color {
        match self {
            PowerStatus::Unconnected => Color::srgba(0.55, 0.58, 0.62, 0.9),
            PowerStatus::NoGenerator => Color::srgba(0.95, 0.35, 0.3, 0.9),
            PowerStatus::Shed => Color::srgba(1.0, 0.7, 0.2, 0.9),
            PowerStatus::Powered => Color::srgba(0.35, 1.0, 0.55, 0.9),
        }
    }
}

/// Dense underfloor cable layer. `true` = cable present on that tile.
/// Mutating it bumps `version` so the overlay knows when to redraw.
#[derive(Resource)]
pub struct CableGrid {
    pub width: i32,
    pub height: i32,
    cells: Vec<bool>,
    pub version: u64,
}

impl CableGrid {
    pub fn new(width: i32, height: i32) -> Self {
        Self {
            width,
            height,
            cells: vec![false; (width * height) as usize],
            version: 0,
        }
    }

    pub fn in_bounds(&self, p: TilePos) -> bool {
        p.x >= 0 && p.y >= 0 && p.x < self.width && p.y < self.height
    }

    pub fn has(&self, p: TilePos) -> bool {
        self.in_bounds(p) && self.cells[(p.y * self.width + p.x) as usize]
    }

    /// Place or remove a cable tile. Returns true when the grid changed.
    pub fn set(&mut self, p: TilePos, present: bool) -> bool {
        if !self.in_bounds(p) || self.cells[(p.y * self.width + p.x) as usize] == present {
            return false;
        }
        self.cells[(p.y * self.width + p.x) as usize] = present;
        self.version += 1;
        true
    }

    pub fn iter_cables(&self) -> impl Iterator<Item = TilePos> + '_ {
        let w = self.width;
        self.cells
            .iter()
            .enumerate()
            .filter(|(_, c)| **c)
            .map(move |(i, _)| TilePos::new(i as i32 % w, i as i32 / w))
    }
}

/// Derived per-network summary for UI/diagnostics.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct NetworkInfo {
    pub generation: u32,
    pub demand: u32,
    pub served: u32,
    pub generators: u32,
    pub consumers: u32,
}

impl NetworkInfo {
    pub fn headroom(self) -> i64 {
        self.generation as i64 - self.demand as i64
    }

    pub fn status_label(self) -> &'static str {
        if self.generators == 0 {
            "No generator"
        } else if self.generation == 0 {
            "No generation (standby)"
        } else if self.demand > self.generation {
            "Insufficient power"
        } else {
            "Stable"
        }
    }

    /// One-line summary safe for the player-facing HUD.
    pub fn summary(self) -> String {
        if self.generators == 0 {
            format!("gen 0 dem {} — no generator", self.demand)
        } else if self.demand > self.generation {
            format!(
                "gen {} dem {} served {} — DEFICIT {}",
                self.generation,
                self.demand,
                self.served,
                self.demand - self.served
            )
        } else {
            format!(
                "gen {} dem {} served {} — headroom {}",
                self.generation,
                self.demand,
                self.served,
                self.generation - self.demand
            )
        }
    }
}

/// Network id (index into `networks`) each cable tile belongs to, plus the
/// per-network summaries. Recomputed every frame by `power_network_system`.
#[derive(Resource, Default, Debug)]
pub struct PowerState {
    pub networks: Vec<NetworkInfo>,
    /// Which network each attached device belongs to (diagnostics / UI).
    pub device_net: HashMap<Entity, usize>,
}

/// Flood-fill the cable layer into connected regions. Returns the region id
/// per cable tile.
pub fn flood_regions(cables: &CableGrid) -> HashMap<TilePos, usize> {
    let mut region_of: HashMap<TilePos, usize> = HashMap::new();
    let mut next_id = 0usize;
    for start in cables.iter_cables() {
        if region_of.contains_key(&start) {
            continue;
        }
        let mut stack = vec![start];
        region_of.insert(start, next_id);
        while let Some(p) = stack.pop() {
            for n in [
                TilePos::new(p.x + 1, p.y),
                TilePos::new(p.x - 1, p.y),
                TilePos::new(p.x, p.y + 1),
                TilePos::new(p.x, p.y - 1),
            ] {
                if cables.has(n) && !region_of.contains_key(&n) {
                    region_of.insert(n, next_id);
                    stack.push(n);
                }
            }
        }
        next_id += 1;
    }
    region_of
}

/// The device-side power interface: footprint tiles plus their 4-adjacent
/// perimeter. A device attaches to the network of any cable found there.
pub fn interface_tiles(map: &ShipMap, foot: &Footprint) -> Vec<TilePos> {
    let mut out: Vec<TilePos> = foot.tiles().collect();
    for t in foot.tiles() {
        for n in [
            TilePos::new(t.x + 1, t.y),
            TilePos::new(t.x - 1, t.y),
            TilePos::new(t.x, t.y + 1),
            TilePos::new(t.x, t.y - 1),
        ] {
            if map.in_bounds(n) && !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out
}

/// A device's network id, if any cable touches its power interface.
pub fn network_of(
    cables: &CableGrid,
    region_of: &HashMap<TilePos, usize>,
    map: &ShipMap,
    foot: &Footprint,
) -> Option<usize> {
    interface_tiles(map, foot)
        .into_iter()
        .filter(|t| cables.has(*t))
        .find_map(|t| region_of.get(&t).copied())
}

/// Recompute the whole power model from the live cable grid. Cheap at starter
/// ship scale (~700 tiles) and impossible to leave stale.
pub fn power_network_system(
    map: Res<ShipMap>,
    cables: Res<CableGrid>,
    mut state: ResMut<PowerState>,
    mut devices: Query<(Entity, &Footprint, &PowerRole, &mut PowerStatus)>,
) {
    let region_of = flood_regions(&cables);
    let raw_count = region_of
        .values()
        .copied()
        .max()
        .map(|m| m + 1)
        .unwrap_or(0);

    // A device whose interface touches several cable groups electrically
    // joins them: union the groups (union-find), then renumber.
    let mut parent: Vec<usize> = (0..raw_count).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    let union = |parent: &mut Vec<usize>, a: usize, b: usize| {
        let (ra, rb) = (find(parent, a), find(parent, b));
        if ra != rb {
            parent[rb] = ra;
        }
    };
    for (_e, foot, _role, _status) in devices.iter() {
        let mut touched: Vec<usize> = interface_tiles(&map, foot)
            .into_iter()
            .filter(|t| cables.has(*t))
            .filter_map(|t| region_of.get(&t).copied())
            .collect();
        // Sort + dedup first: the same region may appear several times
        // non-consecutively, and the union below only pairs against
        // touched[0] — a raw dedup would leave pairs unmerged ([0,1,0]
        // must become [0,1], then 0↔1 unions).
        touched.sort_unstable();
        touched.dedup();
        for i in 1..touched.len() {
            union(&mut parent, touched[0], touched[i]);
        }
    }
    let mut remap: Vec<Option<usize>> = vec![None; raw_count];
    let mut net_count = 0usize;
    for r in 0..raw_count {
        let root = find(&mut parent, r);
        if remap[root].is_none() {
            remap[root] = Some(net_count);
            net_count += 1;
        }
        remap[r] = remap[root];
    }
    let net_of_tile = |t: TilePos| -> Option<usize> { region_of.get(&t).and_then(|r| remap[*r]) };
    let mut networks: Vec<NetworkInfo> = vec![NetworkInfo::default(); net_count];

    // Attach devices and accumulate generation / demand per network.
    struct Attach {
        net: usize,
        entity: Entity,
        role: PowerRole,
    }
    let mut attached: Vec<Attach> = Vec::new();
    for (e, foot, role, mut status) in devices.iter_mut() {
        let interface_nets: Vec<usize> = interface_tiles(&map, foot)
            .into_iter()
            .filter(|t| cables.has(*t))
            .filter_map(net_of_tile)
            .collect();
        match interface_nets.first().copied() {
            Some(net) => {
                match *role {
                    PowerRole::Generator { output, on } => {
                        if on {
                            networks[net].generation += output;
                        }
                        networks[net].generators += 1;
                    }
                    PowerRole::Consumer { demand } => {
                        networks[net].demand += demand;
                        networks[net].consumers += 1;
                    }
                }
                attached.push(Attach {
                    net,
                    entity: e,
                    role: *role,
                });
            }
            None => {
                *status = PowerStatus::Unconnected;
            }
        }
    }

    // Deterministic load shedding: serve consumers in entity-id order (≈
    // build order — older devices keep power), generators always run.
    attached.sort_by_key(|a| a.entity);
    let mut capacity_left: Vec<i64> = networks.iter().map(|n| n.generation as i64).collect();
    let mut final_status: HashMap<Entity, PowerStatus> = HashMap::new();
    for a in &attached {
        match a.role {
            PowerRole::Generator { .. } => {
                final_status.insert(a.entity, PowerStatus::Powered);
            }
            PowerRole::Consumer { demand } => {
                let st = if networks[a.net].generation == 0 {
                    // No output at all: no generator, or only standby ones.
                    PowerStatus::NoGenerator
                } else if capacity_left[a.net] >= demand as i64 {
                    capacity_left[a.net] -= demand as i64;
                    networks[a.net].served += demand;
                    PowerStatus::Powered
                } else {
                    PowerStatus::Shed
                };
                final_status.insert(a.entity, st);
            }
        }
    }
    // Write statuses (separate loop to keep the first borrow simple).
    let mut device_net = HashMap::new();
    for a in &attached {
        device_net.insert(a.entity, a.net);
    }
    for (e, _, _, mut status) in devices.iter_mut() {
        if let Some(st) = final_status.get(&e) {
            *status = *st;
        }
    }
    state.networks = networks;
    state.device_net = device_net;
}

pub struct PowerPlugin;

impl Plugin for PowerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PowerState>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(width: i32, height: i32, cables: &[(i32, i32)]) -> CableGrid {
        let mut g = CableGrid::new(width, height);
        for &(x, y) in cables {
            g.set(TilePos::new(x, y), true);
        }
        g
    }

    #[test]
    fn cable_grid_set_and_version() {
        let mut g = CableGrid::new(4, 4);
        assert!(!g.has(TilePos::new(1, 1)));
        assert!(g.set(TilePos::new(1, 1), true));
        assert!(g.has(TilePos::new(1, 1)));
        assert_eq!(g.version, 1);
        assert!(!g.set(TilePos::new(1, 1), true), "idempotent set");
        assert_eq!(g.version, 1);
        assert!(!g.set(TilePos::new(9, 9), true), "out of bounds rejected");
        assert!(g.set(TilePos::new(1, 1), false));
        assert!(!g.has(TilePos::new(1, 1)));
    }

    #[test]
    fn flood_regions_counts_components() {
        // Two separate runs + one diagonal pair (not connected, 4-dir).
        let g = grid(8, 4, &[(0, 0), (1, 0), (2, 0), (5, 2), (6, 3)]);
        let regions = flood_regions(&g);
        let ids: std::collections::HashSet<usize> = regions.values().copied().collect();
        assert_eq!(ids.len(), 3, "5 tiles in 3 regions, got {regions:?}");
        assert_eq!(regions[&TilePos::new(0, 0)], regions[&TilePos::new(2, 0)]);
        assert_ne!(regions[&TilePos::new(5, 2)], regions[&TilePos::new(6, 3)]);
    }

    #[test]
    fn interface_tiles_include_perimeter() {
        let map = ShipMap::from_layout(&["####", "#..#", "####"]).0;
        let foot = Footprint::new(1, 1, 1, 1);
        let tiles = interface_tiles(&map, &foot);
        assert!(tiles.contains(&TilePos::new(1, 1)));
        assert!(tiles.contains(&TilePos::new(2, 1)));
        assert!(!tiles.contains(&TilePos::new(9, 9)));
    }
}
