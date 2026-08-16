//! Ship thermal model: heat as a real, conserved, spatially-located quantity.
//!
//! Boundary (per the Slice 3 design): devices are ECS entities, but *heat*
//! lives in a dense per-tile grid parallel to `ShipMap` — never one entity
//! per tile. Every open tile (floor / door / machine footprint) carries an
//! ambient air temperature with a small heat capacity; wall tiles carry a
//! structural temperature with a large capacity. Temperature is not heat:
//! every exchange moves an explicit amount of heat `Q` between lumped masses
//! (`Q = cap · ΔT`), so energy is conserved by construction.
//!
//! The only way heat leaves the ship is a coolant radiator dumping into space
//! (see `coolant.rs`) — there is no ambient→space leakage anywhere here.
//!
//! Activity model: each tile has a wake counter. Tiles wake when a device
//! injects heat into them or when a neighbour exchange moves more than
//! `WAKE_EPS`; they fall asleep after `WAKE_STEPS` quiet steps. A uniform
//! ship at rest therefore costs ~nothing per step, and a spreading hot plume
//! wakes exactly the tiles it touches.

use crate::building::{Building, BuildingKind, Footprint};
use crate::map::{ShipMap, Tile, TilePos};
use bevy::prelude::*;
use std::collections::{HashMap, HashSet};

// =====================================================================================
// Tunables (sim seconds, heat units "H", temperatures in °C)
// =====================================================================================

/// Comfortable ship interior at boot.
pub const AMBIENT_START: f32 = 21.0;
/// Deep-space reference temperature (documentation / balance only; the
/// radiator setpoint, not this, gates dumping).
pub const SPACE_TEMP: f32 = -270.0;
/// °C zero point for absolute heat accounting (conservation deltas are
/// unaffected by this offset; it exists so store sums are physically honest).
pub const KELVIN_OFFSET: f32 = 273.15;

/// Heat capacity of one open tile's air + light furnishings (H/K).
pub const AMB_CAP: f32 = 24.0;
/// Structural heat capacity of one hull-wall tile (H/K).
pub const HULL_WALL_CAP: f32 = 80.0;
/// Structural heat capacity of one player-built wall tile (H/K).
pub const BUILT_WALL_CAP: f32 = 60.0;

/// Conduction between 4-adjacent open tiles (H per K per sim second).
pub const K_AIR_AIR: f32 = 22.0;
/// Conduction between an open tile and an adjacent solid tile. The hull is
/// insulated against space: rooms hold their heat, local hotspots form, and
/// a broken cooling loop is a *room-scale* crisis within minutes.
pub const K_AIR_SOLID: f32 = 0.35;
/// Conduction through solid material (wall to wall) — very slow.
pub const K_SOLID_SOLID: f32 = 0.25;

/// Steps a tile stays awake after its last significant exchange.
pub const WAKE_STEPS: u32 = 600;
/// Temperature change (K per step) that counts as "still interesting".
pub const WAKE_EPS: f32 = 0.002;

// ---- reactors -----------------------------------------------------------------------

/// Idle heat of an online reactor (H/s at zero load).
pub const REACTOR_IDLE_HEAT: f32 = 48.0;
/// Extra heat at *rated full load* (H/s). Heat follows the power actually
/// served, not the nameplate — a derated or idle reactor runs cool.
pub const REACTOR_LOAD_HEAT: f32 = 480.0;
/// Ambient temperature at the reactor footprint that trips Overheat.
pub const REACTOR_OVERHEAT_AT: f32 = 80.0;
/// Ambient temperature that trips Critical (emergency power only).
pub const REACTOR_CRITICAL_AT: f32 = 120.0;
/// Hysteresis: Overheat clears only below this.
pub const REACTOR_RECOVER_OVER: f32 = 65.0;
/// Hysteresis: Critical clears only below this (down to Overheat).
pub const REACTOR_RECOVER_CRIT: f32 = 100.0;
/// Generation factor while Overheated.
pub const REACTOR_DERATE: f32 = 0.6;
/// Extra heat an *overheated* core makes while running damaged (H/s) — the
/// derate alone would stabilize the crisis; this guarantees escalation to
/// Critical when cooling stays broken (and a hot core recovers quickly once
/// the loop works again).
pub const REACTOR_OVERHEAT_PENALTY: f32 = 620.0;

// ---- fabricators --------------------------------------------------------------------

/// Heat of a fabricator while a crew member is running a cycle (H/s).
pub const FAB_HEAT: f32 = 24.0;
pub const FAB_OVERHEAT_AT: f32 = 75.0;
pub const FAB_CRITICAL_AT: f32 = 105.0;
pub const FAB_RECOVER_OVER: f32 = 60.0;
pub const FAB_RECOVER_CRIT: f32 = 90.0;
/// Work-speed factor while Overheated (Critical stops work outright).
pub const FAB_OVER_WORK: f32 = 0.4;

// ---- pumps --------------------------------------------------------------------------

/// Heat of a running coolant pump (H/s).
pub const PUMP_HEAT: f32 = 4.0;
/// Pumps must survive anything short of a melting ship: thresholds far above
/// reactor/fabricator ones so coolant keeps moving during a crisis.
pub const PUMP_OVERHEAT_AT: f32 = 200.0;
pub const PUMP_CRITICAL_AT: f32 = 280.0;
pub const PUMP_RECOVER_OVER: f32 = 180.0;
pub const PUMP_RECOVER_CRIT: f32 = 250.0;

// =====================================================================================
// Grid
// =====================================================================================

/// Dense thermal state: ambient air temps for open tiles, structural temps
/// for wall tiles, plus the wake/sleep bookkeeping.
#[derive(Resource)]
pub struct ThermalGrid {
    pub width: i32,
    pub height: i32,
    /// Ambient air temperature per tile (meaningful on open tiles).
    pub amb: Vec<f32>,
    /// Structural temperature per tile (meaningful where `solid_cap > 0`).
    pub solid_temp: Vec<f32>,
    /// Structural heat capacity per tile; 0.0 = open tile (air only).
    pub solid_cap: Vec<f32>,
    wake: Vec<u32>,
    /// Worklist of awake tile indices (exactly those with `wake > 0`).
    awake: Vec<usize>,
}

impl ThermalGrid {
    pub fn new(map: &ShipMap) -> Self {
        let n = (map.width * map.height) as usize;
        let mut solid_cap = vec![0.0f32; n];
        for (p, tile) in map.iter_tiles() {
            let i = (p.y * map.width + p.x) as usize;
            solid_cap[i] = match tile {
                Tile::Wall => HULL_WALL_CAP,
                Tile::BuiltWall => BUILT_WALL_CAP,
                Tile::Floor | Tile::Door | Tile::Machine => 0.0,
            };
        }
        Self {
            width: map.width,
            height: map.height,
            amb: vec![AMBIENT_START; n],
            solid_temp: vec![AMBIENT_START; n],
            solid_cap,
            wake: vec![0; n],
            awake: Vec::new(),
        }
    }

    pub fn idx(&self, p: TilePos) -> usize {
        (p.y * self.width + p.x) as usize
    }

    pub fn pos(&self, i: usize) -> TilePos {
        TilePos::new(i as i32 % self.width, i as i32 / self.width)
    }

    pub fn in_bounds(&self, p: TilePos) -> bool {
        p.x >= 0 && p.y >= 0 && p.x < self.width && p.y < self.height
    }

    pub fn amb_at(&self, p: TilePos) -> f32 {
        if self.in_bounds(p) {
            self.amb[self.idx(p)]
        } else {
            AMBIENT_START
        }
    }

    pub fn is_solid_at(&self, p: TilePos) -> bool {
        self.in_bounds(p) && self.solid_cap[self.idx(p)] > 0.0
    }

    /// Mark a tile thermally interesting for another `WAKE_STEPS`.
    pub fn wake(&mut self, i: usize) {
        if self.wake[i] == 0 {
            self.awake.push(i);
        }
        self.wake[i] = WAKE_STEPS;
    }

    pub fn wake_at(&mut self, p: TilePos) {
        if self.in_bounds(p) {
            self.wake(self.idx(p));
        }
    }

    pub fn is_awake(&self, i: usize) -> bool {
        self.wake[i] > 0
    }

    /// Take the current awake worklist (callers process it, then
    /// `finish_step` re-queues survivors and decrements counters).
    pub fn take_awake(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.awake)
    }

    /// After processing `current`: decrement wake counters, keep tiles that
    /// are still awake. Newly woken tiles are already in `self.awake`.
    pub fn finish_step(&mut self, current: Vec<usize>) {
        for i in current {
            self.wake[i] = self.wake[i].saturating_sub(1);
            if self.wake[i] > 0 {
                self.awake.push(i);
            }
        }
    }

    pub fn awake_count(&self) -> usize {
        self.awake.len()
    }

    /// Effective air heat capacity of an open tile (base + device mass).
    pub fn air_cap(&self, extra_mass: f32) -> f32 {
        AMB_CAP + extra_mass
    }

    /// Total heat content vs absolute zero, including per-tile device mass.
    /// Conservation tests compare *deltas* of this against injections and
    /// radiator dumps.
    pub fn total_heat(&self, devices: &DeviceTiles) -> f64 {
        let mut total = 0.0f64;
        for i in 0..self.amb.len() {
            total +=
                (self.air_cap(devices.mass_at(i)) as f64) * (self.amb[i] + KELVIN_OFFSET) as f64;
            total += (self.solid_cap[i] as f64) * (self.solid_temp[i] + KELVIN_OFFSET) as f64;
        }
        total
    }

    /// Highest ambient temperature over a footprint (device state input).
    pub fn max_footprint_temp(&self, foot: &Footprint) -> f32 {
        foot.tiles()
            .map(|t| self.amb_at(t))
            .fold(f32::NEG_INFINITY, f32::max)
    }

    /// Tile-type conversion (wall built / torn down). The new phase *keeps
    /// the tile's current temperature*: the crew brings wall material in at
    /// room temperature and the displaced air is a rounding error. This is a
    /// deliberate, documented exception to strict heat conservation across
    /// discrete building events — the continuous exchange loop conserves
    /// exactly, and conversions never create a temperature jump (so they can
    /// never act as a free heater or cooler).
    pub fn tile_changed(&mut self, p: TilePos, new_tile: Tile) {
        if !self.in_bounds(p) {
            return;
        }
        let i = self.idx(p);
        let new_cap = match new_tile {
            Tile::Wall => HULL_WALL_CAP,
            Tile::BuiltWall => BUILT_WALL_CAP,
            Tile::Floor | Tile::Door | Tile::Machine => 0.0,
        };
        self.solid_cap[i] = new_cap;
        self.solid_temp[i] = self.amb[i];
        self.wake(i);
    }
}

/// Move heat between two lumped masses, equilibrium-safe for any dt: the
/// amount moved can never exceed what equalizes the pair. Returns the
/// temperature change applied to `a` (wake-decision signal).
pub fn conduct(ta: &mut f32, ca: f32, tb: &mut f32, cb: f32, k: f32, dt: f32) -> f32 {
    let d = *ta - *tb;
    if d == 0.0 || ca <= 0.0 || cb <= 0.0 {
        return 0.0;
    }
    // Heat that would equalize the pair exactly (weighted by capacity).
    let q_eq = d * (ca * cb) / (ca + cb);
    let raw = k * d * dt;
    let q = if raw.abs() > q_eq.abs() { q_eq } else { raw };
    *ta -= q / ca;
    *tb += q / cb;
    (q / ca).abs()
}

// =====================================================================================
// ECS side
// =====================================================================================

/// Device-side thermal identity: extra heat mass spread over the footprint
/// plus the state thresholds for this device class.
#[derive(Component, Clone, Copy, Debug)]
pub struct ThermalBody {
    /// Extra heat capacity (H/K) added to the footprint tiles' air.
    pub mass: f32,
    pub overheat_at: f32,
    pub critical_at: f32,
    pub recover_from_overheat: f32,
    pub recover_from_critical: f32,
}

impl ThermalBody {
    pub fn reactor() -> Self {
        Self {
            mass: 40.0,
            overheat_at: REACTOR_OVERHEAT_AT,
            critical_at: REACTOR_CRITICAL_AT,
            recover_from_overheat: REACTOR_RECOVER_OVER,
            recover_from_critical: REACTOR_RECOVER_CRIT,
        }
    }

    pub fn fabricator() -> Self {
        Self {
            mass: 30.0,
            overheat_at: FAB_OVERHEAT_AT,
            critical_at: FAB_CRITICAL_AT,
            recover_from_overheat: FAB_RECOVER_OVER,
            recover_from_critical: FAB_RECOVER_CRIT,
        }
    }

    pub fn pump() -> Self {
        Self {
            mass: 6.0,
            overheat_at: PUMP_OVERHEAT_AT,
            critical_at: PUMP_CRITICAL_AT,
            recover_from_overheat: PUMP_RECOVER_OVER,
            recover_from_critical: PUMP_RECOVER_CRIT,
        }
    }

    /// Small passive bodies for coolant hardware that neither heats nor trips.
    pub fn passive(mass: f32) -> Self {
        Self {
            mass,
            overheat_at: 200.0,
            critical_at: 280.0,
            recover_from_overheat: 180.0,
            recover_from_critical: 250.0,
        }
    }
}

/// Thermal operating state of one device, with hysteresis (written by
/// `thermal_state_system`, read by power derating and production).
#[derive(Component, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum ThermalState {
    #[default]
    Normal,
    /// Derated but running.
    Overheat,
    /// Emergency: reactors make emergency power only, fabricators stop.
    Critical,
}

impl ThermalState {
    pub fn label(self) -> &'static str {
        match self {
            ThermalState::Normal => "normal",
            ThermalState::Overheat => "OVERHEAT",
            ThermalState::Critical => "THERMAL CRITICAL",
        }
    }

    pub fn color(self) -> Color {
        match self {
            ThermalState::Normal => Color::srgba(0.35, 1.0, 0.55, 0.9),
            ThermalState::Overheat => Color::srgba(1.0, 0.7, 0.2, 0.9),
            ThermalState::Critical => Color::srgba(0.95, 0.3, 0.25, 0.95),
        }
    }

    /// Fabricator work-speed factor under this state.
    pub fn work_factor(self) -> f32 {
        match self {
            ThermalState::Normal => 1.0,
            ThermalState::Overheat => FAB_OVER_WORK,
            ThermalState::Critical => 0.0,
        }
    }
}

/// Per-tile device data rebuilt each step by `thermal_air_system` and shared
/// with the coolant system (HX exchanges against the same effective air cap).
/// Dense (grid-length) so the conduction inner loop reads it with a plain
/// index instead of hashing.
#[derive(Resource, Default, Debug, Clone)]
pub struct DeviceTiles {
    /// Extra heat mass per tile index (device footprint mass, spread).
    /// Empty until the first step sizes it; `mass_at` tolerates that.
    pub mass: Vec<f32>,
}

impl DeviceTiles {
    /// Zeroed, grid-sized device mass (setup + test harnesses).
    pub fn sized(tiles: usize) -> Self {
        Self {
            mass: vec![0.0; tiles],
        }
    }

    /// Extra device mass at a tile (0.0 when unsized or no device).
    #[inline]
    pub fn mass_at(&self, i: usize) -> f32 {
        self.mass.get(i).copied().unwrap_or(0.0)
    }

    /// Reset all masses to zero, keeping the allocation.
    fn reset(&mut self, tiles: usize) {
        if self.mass.len() != tiles {
            self.mass = vec![0.0; tiles];
        } else {
            self.mass.iter_mut().for_each(|m| *m = 0.0);
        }
    }
}

/// Cumulative thermal accounting + per-step telemetry.
#[derive(Resource, Default, Debug, Clone)]
pub struct ThermalStats {
    /// Total heat injected by devices since boot (H).
    pub injected_total: f64,
    /// Total heat dumped overboard by radiators since boot (H) — written by
    /// the coolant system; the ONLY permitted heat exit.
    pub radiated_total: f64,
    /// Awake tiles in the last step (perf telemetry).
    pub active_tiles: usize,
}

/// Step the ambient/structure conduction grid: inject device heat, then
/// conduct between awake tiles and their neighbours. Runs every sim step,
/// after power (needs served load) and before the coolant loop.
#[allow(clippy::type_complexity)]
pub fn thermal_air_system(
    mut grid: ResMut<ThermalGrid>,
    power: Res<crate::power::PowerState>,
    clock: Res<crate::simtime::SimClock>,
    mut devices: ResMut<DeviceTiles>,
    mut stats: ResMut<ThermalStats>,
    heat_sources: Query<(
        Entity,
        &Footprint,
        &Building,
        &ThermalBody,
        &crate::power::PowerStatus,
        Option<&crate::power::PowerRole>,
        Option<&crate::production::Fabricator>,
        Option<&ThermalState>,
    )>,
) {
    let dt = clock.dt() as f32;
    if dt <= 0.0 {
        return;
    }
    devices.reset(grid.amb.len());

    // Online generators per power network: a network's served load is shared
    // between its reactors (single-reactor networks divide by one).
    let mut gens_per_net: HashMap<usize, u32> = HashMap::new();
    for (e, _, building, _, _, role, _, _) in heat_sources.iter() {
        if building.kind == BuildingKind::Reactor
            && matches!(
                role,
                Some(crate::power::PowerRole::Generator { on: true, .. })
            )
        {
            if let Some(&net) = power.device_net.get(&e) {
                *gens_per_net.entry(net).or_insert(0) += 1;
            }
        }
    }

    // ---- 1. Device heat injection + footprint mass -------------------------------
    for (e, foot, building, body, _status, role, fab, tstate) in heat_sources.iter() {
        let tiles: Vec<TilePos> = foot.tiles().collect();
        let per_tile_mass = body.mass / tiles.len() as f32;
        let heat = match building.kind {
            BuildingKind::Reactor => {
                // An online core makes idle heat even with no cables; load
                // heat follows the power actually drawn from its network.
                let on = matches!(
                    role,
                    Some(crate::power::PowerRole::Generator { on: true, .. })
                );
                if !on {
                    0.0
                } else {
                    let net = power
                        .device_net
                        .get(&e)
                        .and_then(|&n| power.networks.get(n));
                    let served = net.map(|n| n.served).unwrap_or(0) as f32;
                    let gens = net
                        .and_then(|_| power.device_net.get(&e).copied())
                        .map(|n| gens_per_net.get(&n).copied().unwrap_or(1))
                        .unwrap_or(1) as f32;
                    let load =
                        (served / (crate::power::REACTOR_OUTPUT as f32 * gens)).clamp(0.0, 1.0);
                    // Damaged-core runaway heat scales with how far past
                    // the safe band the core is: full penalty at Critical's
                    // edge, fading to zero at the recovery threshold, so a
                    // repaired loop always nets cooling (no stall inside the
                    // hysteresis band).
                    let t_now = grid.max_footprint_temp(foot);
                    let severity = ((t_now - REACTOR_RECOVER_OVER)
                        / (REACTOR_CRITICAL_AT - REACTOR_RECOVER_OVER))
                        .clamp(0.0, 1.0);
                    let penalty = match tstate {
                        Some(ThermalState::Overheat) => REACTOR_OVERHEAT_PENALTY * severity,
                        // Critical = emergency power only: barely any load,
                        // no penalty — the core can cool back down.
                        _ => 0.0,
                    };
                    REACTOR_IDLE_HEAT + REACTOR_LOAD_HEAT * load + penalty
                }
            }
            BuildingKind::Fabricator => match fab {
                Some(f) if f.active && _status.ok() => FAB_HEAT,
                _ => 0.0,
            },
            BuildingKind::Pump => {
                if _status.ok() {
                    PUMP_HEAT
                } else {
                    0.0
                }
            }
            _ => 0.0,
        };
        for &t in &tiles {
            let i = grid.idx(t);
            devices.mass[i] += per_tile_mass;
            if heat > 0.0 {
                let q = heat * dt / tiles.len() as f32;
                grid.amb[i] += q / grid.air_cap(per_tile_mass);
                stats.injected_total += q as f64;
                grid.wake(i);
            }
        }
    }

    // ---- 2. Conduction over awake tiles -------------------------------------------
    let current = grid.take_awake();
    stats.active_tiles = current.len();
    let dev_mass = &devices.mass;
    for &i in &current {
        let p = grid.pos(i);
        let solid_i = grid.solid_cap[i] > 0.0;
        let mass_i = dev_mass[i];
        for nb in [
            TilePos::new(p.x + 1, p.y),
            TilePos::new(p.x - 1, p.y),
            TilePos::new(p.x, p.y + 1),
            TilePos::new(p.x, p.y - 1),
        ] {
            if !grid.in_bounds(nb) {
                continue;
            }
            let j = grid.idx(nb);
            // Each unordered pair exactly once per step: an awake tile
            // handles a neighbour if that neighbour is asleep, or if the
            // neighbour's index is higher (both-awake pairs run from the
            // lower index only).
            if grid.is_awake(j) && j < i {
                continue;
            }
            let solid_j = grid.solid_cap[j] > 0.0;
            let moved = match (solid_i, solid_j) {
                (false, false) => {
                    let mj = dev_mass[j];
                    let mut ta = grid.amb[i];
                    let mut tb = grid.amb[j];
                    let moved = conduct(
                        &mut ta,
                        grid.air_cap(mass_i),
                        &mut tb,
                        grid.air_cap(mj),
                        K_AIR_AIR,
                        dt,
                    );
                    grid.amb[i] = ta;
                    grid.amb[j] = tb;
                    moved
                }
                (true, true) => {
                    let mut ta = grid.solid_temp[i];
                    let mut tb = grid.solid_temp[j];
                    let moved = conduct(
                        &mut ta,
                        grid.solid_cap[i],
                        &mut tb,
                        grid.solid_cap[j],
                        K_SOLID_SOLID,
                        dt,
                    );
                    grid.solid_temp[i] = ta;
                    grid.solid_temp[j] = tb;
                    moved
                }
                (a, _b) => {
                    // One solid, one open: air↔structure surface exchange.
                    let (si, sj) = if a { (i, j) } else { (j, i) };
                    let mut ts = grid.solid_temp[si];
                    let mass_open = dev_mass[sj];
                    let mut to = grid.amb[sj];
                    let moved = conduct(
                        &mut ts,
                        grid.solid_cap[si],
                        &mut to,
                        grid.air_cap(mass_open),
                        K_AIR_SOLID,
                        dt,
                    );
                    grid.solid_temp[si] = ts;
                    grid.amb[sj] = to;
                    moved
                }
            };
            if moved > WAKE_EPS {
                grid.wake(j);
            }
        }
    }
    grid.finish_step(current);
}

/// Update device `ThermalState` from footprint ambient temperature with
/// hysteresis, apply reactor generation derating (including emergency output
/// that keeps coolant pumps alive), and log transitions.
#[allow(clippy::type_complexity)]
pub fn thermal_state_system(
    grid: Res<ThermalGrid>,
    power: Res<crate::power::PowerState>,
    clock: Res<crate::simtime::SimClock>,
    mut log: ResMut<crate::log::EventLog>,
    mut devices: Query<(
        Entity,
        &Footprint,
        &Building,
        &ThermalBody,
        &mut ThermalState,
        Option<&mut crate::power::PowerRole>,
    )>,
    pumps: Query<Entity, With<crate::coolant::Pump>>,
) {
    let now = clock.now();
    // Pumps per power network (for reactor emergency output sizing).
    let pump_ids: HashSet<Entity> = pumps.iter().collect();
    for (entity, foot, building, body, mut state, mut role) in devices.iter_mut() {
        let t = grid.max_footprint_temp(foot);
        let old = *state;
        let next = match old {
            ThermalState::Normal => {
                if t >= body.critical_at {
                    ThermalState::Critical
                } else if t >= body.overheat_at {
                    ThermalState::Overheat
                } else {
                    ThermalState::Normal
                }
            }
            ThermalState::Overheat => {
                if t >= body.critical_at {
                    ThermalState::Critical
                } else if t < body.recover_from_overheat {
                    ThermalState::Normal
                } else {
                    ThermalState::Overheat
                }
            }
            ThermalState::Critical => {
                if t < body.recover_from_critical {
                    ThermalState::Overheat
                } else {
                    ThermalState::Critical
                }
            }
        };
        if next != old {
            let label = crate::building::def(building.kind).label;
            log.push(
                now,
                crate::log::LogKind::Info,
                match next {
                    ThermalState::Overheat => {
                        format!("{label} overheated at {t:.0}°C — derating")
                    }
                    ThermalState::Critical => {
                        format!("{label} THERMAL CRITICAL at {t:.0}°C — emergency mode")
                    }
                    ThermalState::Normal => format!("{label} cooled to {t:.0}°C — recovered"),
                },
            );
        }
        *state = next;

        // Reactor generation follows thermal state. Critical = emergency
        // output sized to keep this network's coolant pumps running (the
        // anti-deadlock guarantee: cooling always recovers).
        if building.kind == BuildingKind::Reactor {
            if let Some(crate::power::PowerRole::Generator { output, .. }) = role.as_deref_mut() {
                let emergency = emergency_output_for(&power, &pump_ids, entity);
                *output = match next {
                    ThermalState::Normal => crate::power::REACTOR_OUTPUT,
                    ThermalState::Overheat => {
                        (crate::power::REACTOR_OUTPUT as f32 * REACTOR_DERATE) as u32
                    }
                    ThermalState::Critical => emergency,
                };
            }
        }
    }
}

/// Emergency reactor output: every coolant pump on the reactor's power
/// network plus a small margin — enough that load shedding (which favours
/// older entities) can never strand the pumps.
pub fn emergency_output_for(
    power: &crate::power::PowerState,
    pump_ids: &HashSet<Entity>,
    reactor: Entity,
) -> u32 {
    let Some(&net) = power.device_net.get(&reactor) else {
        return crate::coolant::PUMP_DEMAND + 4;
    };
    let pumps_here = power
        .device_net
        .iter()
        .filter(|(e, &n)| n == net && pump_ids.contains(*e))
        .count() as u32;
    pumps_here * crate::coolant::PUMP_DEMAND + 4
}

/// Overlay heat-map color for an ambient temperature (render + UI shared).
pub fn heat_color(t: f32) -> Color {
    // Anchor points: (°C, RGB).
    const STOPS: [(f32, [f32; 3]); 6] = [
        (0.0, [0.20, 0.30, 0.85]),
        (15.0, [0.25, 0.65, 0.85]),
        (21.0, [0.20, 0.75, 0.45]),
        (45.0, [0.95, 0.85, 0.25]),
        (75.0, [0.95, 0.50, 0.15]),
        (110.0, [0.90, 0.15, 0.12]),
    ];
    let (mut lo, mut hi) = (STOPS[0], STOPS[STOPS.len() - 1]);
    for w in STOPS.windows(2) {
        if t >= w[0].0 && t <= w[1].0 {
            lo = w[0];
            hi = w[1];
        }
    }
    let f = if hi.0 == lo.0 {
        0.0
    } else {
        ((t - lo.0) / (hi.0 - lo.0)).clamp(0.0, 1.0)
    };
    let c = [
        lo.1[0] + (hi.1[0] - lo.1[0]) * f,
        lo.1[1] + (hi.1[1] - lo.1[1]) * f,
        lo.1[2] + (hi.1[2] - lo.1[2]) * f,
    ];
    Color::srgb(c[0], c[1], c[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_of(rows: &[&str]) -> ShipMap {
        ShipMap::from_layout(rows).0
    }

    /// One conduction pass over the grid, mirroring the system's step 2.
    fn conduction_pass(grid: &mut ThermalGrid, dt: f32) {
        let current = grid.take_awake();
        for &i in &current {
            let p = grid.pos(i);
            for nb in [
                TilePos::new(p.x + 1, p.y),
                TilePos::new(p.x - 1, p.y),
                TilePos::new(p.x, p.y + 1),
                TilePos::new(p.x, p.y - 1),
            ] {
                if !grid.in_bounds(nb) {
                    continue;
                }
                let j = grid.idx(nb);
                if grid.is_awake(j) && j < i {
                    continue;
                }
                let solid_i = grid.solid_cap[i] > 0.0;
                let solid_j = grid.solid_cap[j] > 0.0;
                let moved = match (solid_i, solid_j) {
                    (false, false) => {
                        let mut ta = grid.amb[i];
                        let mut tb = grid.amb[j];
                        let m = conduct(&mut ta, AMB_CAP, &mut tb, AMB_CAP, K_AIR_AIR, dt);
                        grid.amb[i] = ta;
                        grid.amb[j] = tb;
                        m
                    }
                    (true, true) => {
                        let mut ta = grid.solid_temp[i];
                        let mut tb = grid.solid_temp[j];
                        let m = conduct(
                            &mut ta,
                            grid.solid_cap[i],
                            &mut tb,
                            grid.solid_cap[j],
                            K_SOLID_SOLID,
                            dt,
                        );
                        grid.solid_temp[i] = ta;
                        grid.solid_temp[j] = tb;
                        m
                    }
                    (a, _b) => {
                        let (si, sj) = if a { (i, j) } else { (j, i) };
                        let mut ts = grid.solid_temp[si];
                        let mut to = grid.amb[sj];
                        let m = conduct(
                            &mut ts,
                            grid.solid_cap[si],
                            &mut to,
                            AMB_CAP,
                            K_AIR_SOLID,
                            dt,
                        );
                        grid.solid_temp[si] = ts;
                        grid.amb[sj] = to;
                        m
                    }
                };
                if moved > WAKE_EPS {
                    grid.wake(j);
                }
            }
        }
        grid.finish_step(current);
    }

    #[test]
    fn conduct_equilibrates_without_overshoot() {
        let (mut a, mut b) = (100.0f32, 0.0f32);
        // One gigantic step must not cross equilibrium.
        let moved = conduct(&mut a, 10.0, &mut b, 10.0, 1_000_000.0, 1e9);
        assert!((a - b).abs() < 1e-3, "overshot: {a} vs {b}");
        assert_eq!(a, 50.0);
        assert_eq!(b, 50.0);
        assert!(moved > 0.0);
    }

    #[test]
    fn conduct_moves_heat_not_temperature_when_caps_differ() {
        let (mut a, mut b) = (50.0f32, 0.0f32);
        // a: cap 10, b: cap 30 → equilibrium temp = 12.5.
        conduct(&mut a, 10.0, &mut b, 30.0, 1_000_000.0, 1e9);
        assert!((a - 12.5).abs() < 1e-4);
        assert!((b - 12.5).abs() < 1e-4);
    }

    #[test]
    fn injection_respects_capacity() {
        let map = map_of(&["###", "#.#", "###"]);
        let mut grid = ThermalGrid::new(&map);
        let i = grid.idx(TilePos::new(1, 1));
        grid.amb[i] += 240.0 / AMB_CAP; // 240 H into a 24 H/K tile
        assert!((grid.amb[i] - (AMBIENT_START + 10.0)).abs() < 1e-4);
    }

    #[test]
    fn total_heat_tracks_injection_exactly() {
        let map = map_of(&["####", "#..#", "#..#", "####"]);
        let mut grid = ThermalGrid::new(&map);
        let devices = DeviceTiles::default();
        let before = grid.total_heat(&devices);
        let i = grid.idx(TilePos::new(1, 1));
        let q = 480.0f64;
        grid.amb[i] += (q as f32) / AMB_CAP;
        let after = grid.total_heat(&devices);
        assert!((after - before - q).abs() < 1e-6);
    }

    #[test]
    fn air_conduction_spreads_heat_conserves_and_sleeps() {
        let map = map_of(&["#####", "#...#", "#####"]);
        let mut grid = ThermalGrid::new(&map);
        let (a, b) = (grid.idx(TilePos::new(1, 1)), grid.idx(TilePos::new(3, 1)));
        grid.amb[a] = 60.0;
        grid.amb[b] = 0.0;
        grid.wake(a);
        grid.wake(b);
        let devices = DeviceTiles::default();
        let h0 = grid.total_heat(&devices);
        for _ in 0..4000 {
            conduction_pass(&mut grid, 1.0);
        }
        let h1 = grid.total_heat(&devices);
        // f32 exchange accumulation drifts slightly over hundreds of steps.
        assert!(
            (h1 - h0).abs() < 0.5,
            "heat must be conserved: {h0} vs {h1}"
        );
        // The three air tiles share their heat quickly (walls pull the common
        // level toward their own large mass — the exact mean is wall-weighted,
        // so assert spread, not a number).
        let (t1, t2, t3) = (
            grid.amb_at(TilePos::new(1, 1)),
            grid.amb_at(TilePos::new(2, 1)),
            grid.amb_at(TilePos::new(3, 1)),
        );
        assert!(t1 < 60.0 && t3 > 0.0, "no spread: {t1} {t2} {t3}");
        assert!(
            (t1 - t3).abs() < 1.5 && (t1 - t2).abs() < 1.5,
            "air tiles not equalized: {t1} {t2} {t3}"
        );
        // And then fell asleep again.
        assert_eq!(grid.awake_count(), 0);
    }

    #[test]
    fn sleeping_tiles_do_not_conduct() {
        let map = map_of(&["#######", "#.....#", "#######"]);
        let mut grid = ThermalGrid::new(&map);
        let hot = grid.idx(TilePos::new(1, 1));
        let far = grid.idx(TilePos::new(5, 1));
        grid.amb[hot] = 80.0;
        // Nothing awake: temperatures stay frozen.
        for _ in 0..50 {
            conduction_pass(&mut grid, 1.0);
        }
        assert_eq!(grid.amb[hot], 80.0);
        assert_eq!(grid.amb[far], AMBIENT_START);
        // Waking the hot tile spreads heat outward.
        grid.wake(hot);
        for _ in 0..200 {
            conduction_pass(&mut grid, 1.0);
        }
        assert!(grid.amb[hot] < 80.0);
        assert!(grid.amb[far] > AMBIENT_START);
    }

    #[test]
    fn walls_conduct_slower_than_air() {
        // Two tiles at ΔT=40 with one tile between them: air middle vs wall
        // middle. After the same number of steps the air path must have
        // moved more heat into the far tile.
        let spread = |wall_in_middle: bool| -> f32 {
            let rows = if wall_in_middle {
                ["#####", "#.#.#", "#####"]
            } else {
                ["#####", "#...#", "#####"]
            };
            let map = map_of(&rows);
            let mut g = ThermalGrid::new(&map);
            let (a, b) = (g.idx(TilePos::new(1, 1)), g.idx(TilePos::new(3, 1)));
            g.amb[a] = 60.0;
            g.amb[b] = 20.0;
            g.wake(a);
            g.wake(b);
            for _ in 0..5 {
                conduction_pass(&mut g, 1.0);
            }
            g.amb_at(TilePos::new(3, 1))
        };
        let via_air = spread(false);
        let via_wall = spread(true);
        assert!(
            via_air > via_wall,
            "far tile warmed faster through air ({via_air}) than wall ({via_wall})"
        );
    }

    #[test]
    fn tile_conversion_keeps_temperature() {
        let map = map_of(&["####", "#..#", "####"]);
        let mut grid = ThermalGrid::new(&map);
        let p = TilePos::new(1, 1);
        let i = grid.idx(p);
        grid.amb[i] = 55.0;
        grid.tile_changed(p, Tile::BuiltWall);
        assert!(grid.is_solid_at(p));
        assert!((grid.solid_temp[i] - 55.0).abs() < 1e-4);
        grid.tile_changed(p, Tile::Floor);
        assert!(!grid.is_solid_at(p));
        assert!((grid.amb_at(p) - 55.0).abs() < 1e-4);
    }

    #[test]
    fn heat_color_spans_cold_to_hot() {
        let to_rgb = |c: Color| {
            let Color::Srgba(v) = c else { panic!("srgb") };
            (v.red, v.green, v.blue)
        };
        let (_cr, _cg, cb) = to_rgb(heat_color(-10.0));
        let (hr, _hg, hb) = to_rgb(heat_color(120.0));
        assert!(cb > 0.5, "cold is blue");
        assert!(hr > 0.5, "hot is red");
        assert!(hb < 0.5);
        let (mr, _mg, mb) = to_rgb(heat_color(21.0));
        assert!(mr < 0.6 && mb < 0.6, "room temp is greenish");
    }
}
