//! Coolant loop: a dense underfloor pipe layer with real topology, a finite
//! amount of water carried as per-tile packets (amount + temperature), and
//! passive heat exchangers / radiators.
//!
//! Mirrors the power grid's philosophy: pipes are dense grid data (never
//! entities), devices are ECS entities standing ON pipe tiles, and networks
//! are flood-filled from the live grid every step so edits can never leave
//! the model stale. Water is conserved: circulation only moves it around,
//! pipe removal redistributes it into the network, and the only intentional
//! loss is a documented "spill" when the network genuinely has no room.
//!
//! Heat enters the loop at heat exchangers (air → water, never below the HX
//! engagement setpoint, and never against the temperature gradient — hot
//! water can warm cold air, but nothing here actively refrigerates) and
//! leaves only at radiators (water → space, gated by the radiator setpoint,
//! capped per radiator). A pump is what actually moves packets: without a
//! powered pump on the network the water stagnates and only the local packet
//! at each device exchanges heat.

use crate::building::Footprint;
use crate::map::TilePos;
use crate::thermal::{DeviceTiles, ThermalGrid, ThermalStats};
use bevy::prelude::*;
use std::collections::HashMap;

// =====================================================================================
// Tunables
// =====================================================================================

/// Water units held by one plain pipe tile.
pub const PIPE_TILE_CAP: f32 = 8.0;
/// Extra capacity provided by a reservoir on its pipe tile.
pub const RESERVOIR_ADD_CAP: f32 = 50.0;
/// Heat capacity per water unit (H/K).
pub const WATER_CAP: f32 = 12.0;

/// Circulation driven per powered pump (water units per sim second).
pub const PUMP_FLOW: f32 = 3.4;
/// Network circulation ceiling regardless of pump count.
pub const MAX_FLOW: f32 = 8.0;
/// Power drawn by a running pump.
pub const PUMP_DEMAND: u32 = 6;

/// Heat exchanger air→water conductance (H per K per sim second).
pub const K_HX: f32 = 56.0;
/// HX only picks up heat from air hotter than this (passive thermal-expansion
/// valve: keeps an idle ship from being refrigerated by its own loop).
pub const HX_SETPOINT: f32 = 30.0;

/// Radiator water→space conductance (H per K per sim second).
pub const K_RAD: f32 = 26.0;
/// Radiators stop dumping below this water temperature (bypass setpoint —
/// same anti-freeze rationale as the HX setpoint).
pub const RAD_SETPOINT: f32 = 15.0;
/// Hard cap per radiator: finite, physical, never reset.
pub const RAD_MAX_DUMP: f32 = 900.0;

// =====================================================================================
// Grids
// =====================================================================================

/// Dense underfloor pipe layer. `true` = pipe present on that tile.
/// Mutating it bumps `version` so the overlay knows when to redraw.
#[derive(Resource)]
pub struct PipeGrid {
    pub width: i32,
    pub height: i32,
    cells: Vec<bool>,
    pub version: u64,
}

impl PipeGrid {
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

    /// Place or remove a pipe tile. Returns true when the grid changed.
    pub fn set(&mut self, p: TilePos, present: bool) -> bool {
        if !self.in_bounds(p) || self.cells[(p.y * self.width + p.x) as usize] == present {
            return false;
        }
        self.cells[(p.y * self.width + p.x) as usize] = present;
        self.version += 1;
        true
    }

    pub fn iter_pipes(&self) -> impl Iterator<Item = TilePos> + '_ {
        let w = self.width;
        self.cells
            .iter()
            .enumerate()
            .filter(|(_, c)| **c)
            .map(move |(i, _)| TilePos::new(i as i32 % w, i as i32 / w))
    }

    pub fn idx(&self, p: TilePos) -> usize {
        (p.y * self.width + p.x) as usize
    }
}

/// Water packets: amount + temperature per tile, parallel to `PipeGrid`.
/// Entries are meaningful only where a pipe is present.
#[derive(Resource)]
pub struct WaterGrid {
    pub width: i32,
    pub height: i32,
    pub amount: Vec<f32>,
    pub temp: Vec<f32>,
}

impl WaterGrid {
    pub fn new(width: i32, height: i32) -> Self {
        Self {
            width,
            height,
            amount: vec![0.0; (width * height) as usize],
            temp: vec![crate::thermal::AMBIENT_START; (width * height) as usize],
        }
    }

    pub fn in_bounds(&self, p: TilePos) -> bool {
        p.x >= 0 && p.y >= 0 && p.x < self.width && p.y < self.height
    }

    pub fn amount_at(&self, p: TilePos) -> f32 {
        if self.in_bounds(p) {
            self.amount[(p.y * self.width + p.x) as usize]
        } else {
            0.0
        }
    }

    pub fn temp_at(&self, p: TilePos) -> f32 {
        if self.in_bounds(p) {
            self.temp[(p.y * self.width + p.x) as usize]
        } else {
            crate::thermal::AMBIENT_START
        }
    }

    /// Pre-fill a tile (map setup / tests).
    pub fn fill(&mut self, p: TilePos, amount: f32, temp: f32) {
        if !self.in_bounds(p) {
            return;
        }
        let i = (p.y * self.width + p.x) as usize;
        self.amount[i] = amount;
        self.temp[i] = temp;
    }

    pub fn total_water(&self) -> f32 {
        self.amount.iter().sum()
    }
}

// =====================================================================================
// Devices
// =====================================================================================

/// Circulation pump: a power consumer standing on a pipe tile.
#[derive(Component)]
pub struct Pump;

/// Water tank: adds large storage capacity on its pipe tile.
#[derive(Component)]
pub struct Reservoir;

/// Passive air→water heat exchanger on a pipe tile.
#[derive(Component)]
pub struct HeatExchanger;

/// Radiator: dumps water heat to space. Must be 4-adjacent to a hull wall
/// (validated at placement; hull is permanent so this never goes stale).
#[derive(Component)]
pub struct Radiator {
    pub hull_ok: bool,
}

// =====================================================================================
// Derived networks
// =====================================================================================

/// Per-network summary (UI / diagnostics).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct CoolantNet {
    pub tiles: u32,
    pub water: f32,
    /// Amount-weighted water temperature.
    pub avg_temp: f32,
    pub pumps: u32,
    pub powered_pumps: u32,
    pub exchangers: u32,
    pub radiators: u32,
    /// Circulation this step (water units / s).
    pub flow: f32,
    /// Heat dumped overboard last step (H/s).
    pub dump_rate: f32,
    /// Heat picked up from the air last step (H/s).
    pub pickup_rate: f32,
}

impl CoolantNet {
    pub fn status_label(&self) -> &'static str {
        if self.tiles == 0 {
            "Empty"
        } else if self.pumps == 0 {
            "No pump"
        } else if self.powered_pumps == 0 {
            "Stagnant — pump unpowered"
        } else {
            "Circulating"
        }
    }
}

/// Flood-filled pipe networks, recomputed every step. `order` holds each
/// network's DFS walk (contiguous around loops) — the sequence water
/// circulates along, starting at the first powered pump.
#[derive(Resource, Default, Debug)]
pub struct CoolantState {
    pub networks: Vec<CoolantNet>,
    pub device_net: HashMap<Entity, usize>,
    pub order: Vec<Vec<usize>>,
    /// Topology epoch: `PipeGrid::version` folded with the coolant device
    /// set. While unchanged, the flood fill / attachment work is skipped —
    /// water and temperatures still advance every step.
    pub topo_sig: u64,
    /// Pipe tile index → network index (rebuilt only on topology changes).
    pub net_of: HashMap<usize, usize>,
    /// Telemetry: full topology rebuilds since boot (1 per pipe/device edit).
    pub topology_rebuilds: u32,
    /// Scratch buffer for circulation releases (reused every step).
    released: Vec<f32>,
}

/// Coolant accounting + telemetry.
#[derive(Resource, Default, Debug)]
pub struct CoolantStats {
    /// Water destroyed because the network had no room (pipe teardown).
    pub spilled_water: f32,
    /// Highest network circulation seen (water units / s).
    pub max_flow_seen: f32,
    /// Pipe-tile indices carrying a reservoir's extra capacity this step
    /// (kept fresh by `coolant_system`; teardown uses it to preserve water).
    pub reservoir_tiles: Vec<usize>,
}

/// Total heat content of all water packets (conservation tests).
pub fn water_heat(water: &WaterGrid) -> f64 {
    water
        .amount
        .iter()
        .zip(water.temp.iter())
        .map(|(&a, &t)| (a * WATER_CAP) as f64 * (t + crate::thermal::KELVIN_OFFSET) as f64)
        .sum()
}

/// Attach a device to the network of its (pipe-backed) tile.
fn attach_into(
    device_net: &mut HashMap<Entity, usize>,
    net_of: &HashMap<usize, usize>,
    e: Entity,
    tile: TilePos,
    pipes: &PipeGrid,
) -> Option<usize> {
    if !pipes.has(tile) {
        return None;
    }
    let net = *net_of.get(&pipes.idx(tile))?;
    device_net.insert(e, net);
    Some(net)
}

/// Advance the coolant loop one sim step: derive networks, circulate water
/// along each network's walk order, exchange heat at exchangers and radiators.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
pub fn coolant_system(
    pipes: Res<PipeGrid>,
    mut water: ResMut<WaterGrid>,
    mut thermal: ResMut<ThermalGrid>,
    devices: Res<DeviceTiles>,
    clock: Res<crate::simtime::SimClock>,
    mut state: ResMut<CoolantState>,
    mut tstats: ResMut<ThermalStats>,
    mut cstats: ResMut<CoolantStats>,
    pumps: Query<(Entity, &Footprint, &crate::power::PowerStatus), With<Pump>>,
    exchangers: Query<(Entity, &Footprint), With<HeatExchanger>>,
    radiators: Query<(Entity, &Footprint, &Radiator)>,
    reservoirs: Query<(Entity, &Footprint), With<Reservoir>>,
) {
    let dt = clock.dt() as f32;
    if dt <= 0.0 {
        return;
    }

    // ---- 1. Topology: flood fill + device attachment, cached per epoch -------------
    // Topology only changes when pipes are edited or coolant devices are
    // added/removed — never because water moved or power flipped. Folding the
    // pipe version with the device entity bits gives a cheap per-step guard.
    let mut device_sig: u64 = 0xcbf2_9ce4_8422_2325;
    for (e, _, _) in pumps.iter() {
        device_sig = device_sig.wrapping_mul(31) ^ (e.to_bits() << 3) ^ 0xA1;
    }
    for (e, _) in exchangers.iter() {
        device_sig = device_sig.wrapping_mul(31) ^ (e.to_bits() << 3) ^ 0xB2;
    }
    for (e, _, _) in radiators.iter() {
        device_sig = device_sig.wrapping_mul(31) ^ (e.to_bits() << 3) ^ 0xC3;
    }
    for (e, _) in reservoirs.iter() {
        device_sig = device_sig.wrapping_mul(31) ^ (e.to_bits() << 3) ^ 0xD4;
    }
    let topo_sig = pipes.version.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ device_sig;

    if topo_sig != state.topo_sig {
        state.topo_sig = topo_sig;
        state.topology_rebuilds += 1;
        let mut order: Vec<Vec<usize>> = Vec::new();
        let mut net_of: HashMap<usize, usize> = HashMap::new();
        {
            let mut visited = vec![false; water.amount.len()];
            let mut starts: Vec<usize> = pipes.iter_pipes().map(|p| pipes.idx(p)).collect();
            starts.sort_unstable();
            for start in starts {
                if visited[start] {
                    continue;
                }
                let net = order.len();
                let mut walk: Vec<usize> = Vec::new();
                let mut stack = vec![start];
                visited[start] = true;
                while let Some(i) = stack.pop() {
                    walk.push(i);
                    net_of.insert(i, net);
                    let p = TilePos::new(i as i32 % water.width, i as i32 / water.width);
                    let mut nbs: Vec<usize> = [
                        TilePos::new(p.x + 1, p.y),
                        TilePos::new(p.x - 1, p.y),
                        TilePos::new(p.x, p.y + 1),
                        TilePos::new(p.x, p.y - 1),
                    ]
                    .iter()
                    .filter(|&&n| pipes.has(n))
                    .map(|&n| pipes.idx(n))
                    .filter(|&j| !visited[j])
                    .collect::<Vec<_>>();
                    // Ascending on the walk: push descending so the stack pops
                    // the smallest index first.
                    nbs.sort_unstable();
                    for &j in nbs.iter().rev() {
                        visited[j] = true;
                        stack.push(j);
                    }
                }
                order.push(walk);
            }
        }
        let mut device_net: HashMap<Entity, usize> = HashMap::new();
        for (e, foot, _) in pumps.iter() {
            if let Some(tile) = foot.tiles().next() {
                attach_into(&mut device_net, &net_of, e, tile, &pipes);
            }
        }
        for (e, foot) in exchangers.iter() {
            if let Some(tile) = foot.tiles().next() {
                attach_into(&mut device_net, &net_of, e, tile, &pipes);
            }
        }
        for (e, foot, _) in radiators.iter() {
            if let Some(tile) = foot.tiles().next() {
                attach_into(&mut device_net, &net_of, e, tile, &pipes);
            }
        }
        // Reservoir bonus tiles (read by pipe teardown to preserve water).
        let mut res_tiles: Vec<usize> = Vec::new();
        for (_, foot) in reservoirs.iter() {
            for t in foot.tiles() {
                if pipes.has(t) {
                    res_tiles.push(pipes.idx(t));
                }
            }
        }
        state.order = order;
        state.net_of = net_of;
        state.device_net = device_net;
        cstats.reservoir_tiles = res_tiles;
    }

    // ---- 2. Base summaries (water content changes every step) ---------------------
    let mut nets: Vec<CoolantNet> = state
        .order
        .iter()
        .map(|walk| {
            let (mut w_sum, mut t_sum) = (0.0f32, 0.0f32);
            for &i in walk {
                w_sum += water.amount[i];
                t_sum += water.amount[i] * water.temp[i];
            }
            CoolantNet {
                tiles: walk.len() as u32,
                water: w_sum,
                avg_temp: if w_sum > 0.0 { t_sum / w_sum } else { 0.0 },
                ..CoolantNet::default()
            }
        })
        .collect();
    let mut powered_pumps = vec![0u32; state.order.len()];
    let mut pump_start = vec![None::<usize>; state.order.len()];
    for (e, foot, status) in pumps.iter() {
        let Some(&net) = state.device_net.get(&e) else {
            continue;
        };
        nets[net].pumps += 1;
        if status.ok() {
            nets[net].powered_pumps += 1;
            powered_pumps[net] += 1;
            let i = pipes.idx(foot.tiles().next().unwrap());
            let better = pump_start[net].map(|cur| i < cur).unwrap_or(true);
            if better {
                pump_start[net] = Some(i);
            }
        }
    }
    for (e, _) in exchangers.iter() {
        if let Some(&net) = state.device_net.get(&e) {
            nets[net].exchangers += 1;
        }
    }
    for (e, _, rad) in radiators.iter() {
        if let Some(&net) = state.device_net.get(&e) {
            nets[net].radiators += u32::from(rad.hull_ok);
        }
    }

    // ---- 3. Circulation: simultaneous rotation along each walk --------------------
    // Every tile releases up to `flow` water and receives its predecessor's
    // release — full rings circulate (displacement flow), amounts can never
    // exceed capacity, and water is conserved exactly. Rotation is pure index
    // arithmetic into the cached walk (no per-step allocation).
    // Scratch taken out of `state` so the immutable `order` borrow above can
    // coexist with filling it.
    let mut released = std::mem::take(&mut state.released);
    for (net, walk) in state.order.iter().enumerate() {
        let n = walk.len();
        if n < 2 || powered_pumps[net] == 0 {
            continue;
        }
        let flow = (PUMP_FLOW * powered_pumps[net] as f32).min(MAX_FLOW);
        nets[net].flow = flow;
        cstats.max_flow_seen = cstats.max_flow_seen.max(flow);
        // Rotate the walk so it starts at the first powered pump: packets
        // flow away from the pump along the walk.
        let rot = pump_start[net]
            .and_then(|s| walk.iter().position(|&i| i == s))
            .unwrap_or(0);
        released.clear();
        released.resize(n, 0.0);
        for k in 0..n {
            released[k] = water.amount[walk[(rot + k) % n]].min(flow);
        }
        for k in (0..n).rev() {
            let m = released[k];
            if m <= 0.0 {
                continue;
            }
            let (i, j) = (walk[(rot + k) % n], walk[(rot + k + 1) % n]);
            let (wi, wj) = (water.amount[i], water.amount[j]);
            water.amount[i] = wi - m;
            let tj = if wj + m > 0.0 {
                (water.temp[j] * wj + water.temp[i] * m) / (wj + m)
            } else {
                water.temp[i]
            };
            water.amount[j] = wj + m;
            water.temp[j] = tj;
        }
    }
    state.released = released;

    // ---- 5. Heat exchangers: air ↔ water at their tile ----------------------------
    for (e, foot) in exchangers.iter() {
        let Some(tile) = foot.tiles().next() else {
            continue;
        };
        if !pipes.has(tile) {
            continue;
        }
        let i = pipes.idx(tile);
        let w = water.amount[i];
        if w <= 0.0 {
            continue;
        }
        let ta = thermal.amb[i];
        let tw = water.temp[i];
        let cw = w * WATER_CAP;
        let ca = thermal.air_cap_at(i, devices.mass_at(i));
        // Passive valve: pickup only above the setpoint; hot water may warm
        // cold air; never against the temperature gradient.
        let rate = if tw >= ta {
            K_HX * (ta - tw)
        } else {
            (K_HX * (ta - tw.max(HX_SETPOINT))).max(0.0)
        };
        let mut q = rate * dt;
        // Equilibrium-safe clamp: never move more than would equalize the
        // pair (q and the equalization heat always share a sign here).
        let q_eq = (ta - tw) * (ca * cw) / (ca + cw);
        if q.abs() > q_eq.abs() {
            q = q_eq;
        }
        if q == 0.0 {
            continue;
        }
        thermal.amb[i] = ta - q / ca;
        water.temp[i] = tw + q / cw;
        if let Some(&net) = state.device_net.get(&e) {
            nets[net].pickup_rate += q / dt;
        }
        if (q / ca).abs() > crate::thermal::WAKE_EPS {
            thermal.wake(i);
        }
    }

    // ---- 6. Radiators: water → space ----------------------------------------------
    for (e, foot, rad) in radiators.iter() {
        if !rad.hull_ok {
            continue;
        }
        let Some(tile) = foot.tiles().next() else {
            continue;
        };
        if !pipes.has(tile) {
            continue;
        }
        let i = pipes.idx(tile);
        let w = water.amount[i];
        if w <= 0.0 {
            continue;
        }
        let tw = water.temp[i];
        if tw <= RAD_SETPOINT {
            continue;
        }
        let cw = w * WATER_CAP;
        let rate = (K_RAD * (tw - RAD_SETPOINT)).min(RAD_MAX_DUMP);
        let mut q = rate * dt;
        // Never cool the packet below the setpoint.
        let q_max = (tw - RAD_SETPOINT) * cw;
        if q > q_max {
            q = q_max;
        }
        if q <= 0.0 {
            continue;
        }
        water.temp[i] = tw - q / cw;
        tstats.radiated_total += q as f64;
        if let Some(&net) = state.device_net.get(&e) {
            nets[net].dump_rate += q / dt;
        }
    }

    state.networks = nets;
}

// =====================================================================================
// Edit support
// =====================================================================================

/// Remove a pipe tile, redistributing its water into 4-adjacent pipe
/// neighbours up to their capacity (reservoir bonus included via
/// `reservoirs`). Returns leftover water that could not be stored — the
/// caller logs it as a spill. Heat travels with the water.
pub fn remove_pipe_preserving_water(
    pipes: &mut PipeGrid,
    water: &mut WaterGrid,
    cstats: &mut CoolantStats,
    p: TilePos,
) -> f32 {
    if !pipes.has(p) {
        return 0.0;
    }
    let i = pipes.idx(p);
    let (mut amount, temp) = (water.amount[i], water.temp[i]);
    let mut nbs: Vec<usize> = [
        TilePos::new(p.x + 1, p.y),
        TilePos::new(p.x - 1, p.y),
        TilePos::new(p.x, p.y + 1),
        TilePos::new(p.x, p.y - 1),
    ]
    .iter()
    .filter(|&&n| pipes.has(n))
    .map(|&n| pipes.idx(n))
    .collect();
    nbs.sort_unstable();
    nbs.dedup();
    for &j in &nbs {
        if amount <= 0.0 {
            break;
        }
        let cap_j = if cstats.reservoir_tiles.contains(&j) {
            PIPE_TILE_CAP + RESERVOIR_ADD_CAP
        } else {
            PIPE_TILE_CAP
        };
        let room = (cap_j - water.amount[j]).max(0.0);
        let m = room.min(amount);
        if m > 0.0 {
            let wj = water.amount[j];
            water.temp[j] = if wj + m > 0.0 {
                (water.temp[j] * wj + temp * m) / (wj + m)
            } else {
                temp
            };
            water.amount[j] = wj + m;
            amount -= m;
        }
    }
    water.amount[i] = 0.0;
    water.temp[i] = crate::thermal::AMBIENT_START;
    pipes.set(p, false);
    if amount > 1e-4 {
        cstats.spilled_water += amount;
    }
    amount.max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small_pipes() -> (PipeGrid, WaterGrid) {
        (PipeGrid::new(8, 4), WaterGrid::new(8, 4))
    }

    #[test]
    fn pipe_grid_set_and_version() {
        let (mut g, _) = small_pipes();
        assert!(!g.has(TilePos::new(1, 1)));
        assert!(g.set(TilePos::new(1, 1), true));
        assert!(g.has(TilePos::new(1, 1)));
        assert_eq!(g.version, 1);
        assert!(!g.set(TilePos::new(1, 1), true));
        assert!(!g.set(TilePos::new(9, 9), true));
    }

    #[test]
    fn water_preserved_or_spilled_on_pipe_removal() {
        // Full run: middle tile's water cannot fit anywhere → spill reported.
        let (mut pipes, mut water) = small_pipes();
        for x in 1..=3 {
            pipes.set(TilePos::new(x, 1), true);
            water.fill(TilePos::new(x, 1), PIPE_TILE_CAP, 40.0);
        }
        let before = water.total_water();
        let mut cstats = CoolantStats::default();
        let spilled =
            remove_pipe_preserving_water(&mut pipes, &mut water, &mut cstats, TilePos::new(2, 1));
        assert!(
            spilled > 7.9,
            "full neighbours cannot absorb, got {spilled}"
        );
        assert!((water.total_water() + spilled - before).abs() < 1e-4);

        // Empty run: everything is absorbed, heat travels with the water.
        let (mut pipes, mut water) = small_pipes();
        for x in 1..=3 {
            pipes.set(TilePos::new(x, 1), true);
        }
        water.fill(TilePos::new(2, 1), 6.0, 60.0);
        let before = water.total_water();
        let mut cstats = CoolantStats::default();
        let spilled =
            remove_pipe_preserving_water(&mut pipes, &mut water, &mut cstats, TilePos::new(2, 1));
        assert_eq!(spilled, 0.0);
        assert_eq!(water.total_water(), before, "water preserved");
        assert!(!pipes.has(TilePos::new(2, 1)));
        let carried: f32 = [1, 3]
            .iter()
            .map(|&x| water.amount_at(TilePos::new(x, 1)) * water.temp_at(TilePos::new(x, 1)))
            .sum();
        assert!(carried > 0.0, "heat travelled with the water");
    }

    #[test]
    fn water_heat_sums_packets() {
        let (_, mut water) = small_pipes();
        water.fill(TilePos::new(0, 0), 2.0, 30.0);
        water.fill(TilePos::new(1, 0), 1.0, 60.0);
        let h = water_heat(&water);
        let expect = 2.0_f64 * WATER_CAP as f64 * (30.0 + crate::thermal::KELVIN_OFFSET) as f64
            + WATER_CAP as f64 * (60.0 + crate::thermal::KELVIN_OFFSET) as f64;
        assert!((h - expect).abs() < 1e-2);
    }

    #[test]
    fn radiator_dump_is_finite_and_setpoint_gated() {
        // Formula-level check of the radiator model used by the system.
        let tw = 45.0f32;
        let rate = (K_RAD * (tw - RAD_SETPOINT)).min(RAD_MAX_DUMP);
        assert!((rate - K_RAD * 30.0).abs() < 1e-4);
        // Very hot water hits the physical cap.
        let tw = 200.0;
        let rate = (K_RAD * (tw - RAD_SETPOINT)).min(RAD_MAX_DUMP);
        assert_eq!(rate, RAD_MAX_DUMP);
        // Below the setpoint: nothing.
        let tw = 14.0;
        assert!(K_RAD * (tw - RAD_SETPOINT) <= 0.0);
    }
}
