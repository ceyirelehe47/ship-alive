//! Ventilation & gas handling (Slice 6): the player's engineering tools for
//! moving, storing and controlling the air Slice 5 made real.
//!
//! Architecture — dense infrastructure + ECS devices + derived topology:
//!
//! * `DuctGrid` is a dense underground layer exactly like cables/pipes. Each
//!   duct cell is a **finite gas volume** (`DUCT_MOL` units) holding a real
//!   four-species mixture plus its own temperature/thermal energy. Gas moves
//!   between neighbouring duct cells by the same equilibrium-clamped
//!   pressure rule as the atmosphere — never a network-global average.
//! * `Vent` (room↔duct), `Blower` (directed duct push, power consumer) and
//!   `GasTank` (finite buffer with a valve) are ECS devices standing on duct
//!   tiles.
//! * `DuctTopology` is a derived per-cell network labelling, rebuilt only on
//!   duct edits or device-set changes — gas flow itself never rebuilds it.
//!
//! Gas semantics are **reused, not re-implemented**: species amounts are the
//! authority, pressure is derived through `atmosphere::pressure_vol`, and
//! every transfer goes through `atmosphere::move_gas`, which carries species
//! proportionally and mixes sensible heat by energy conservation. The
//! ventilation system is the single authority for vent transfers (the
//! atmosphere never performs them), so nothing is moved twice.
//!
//! Ventilation transport is a *controlled edge*, not spatial connectivity:
//! two rooms exchanging gas through a duct remain separate structural
//! compartments and separate `air_group`s.

use crate::atmosphere::{
    eq_amount, move_gas, pressure_vol, GasMixture, GAS_CAP_PER_MOL, KELVIN_OFFSET, PRESSURE_REF,
    TEMP_REF,
};
use crate::map::{ShipMap, TilePos};
use bevy::prelude::*;

// =====================================================================================
// Tunables (sim seconds; 60 sim s per real s at 1×)
// =====================================================================================

/// Effective gas volume of one duct cell (mol units at standard fill).
pub const DUCT_MOL: f32 = 10.0;
/// Effective gas volume of one gas tank.
pub const TANK_MOL: f32 = 400.0;
/// Fraction of the pair-equalizing amount exchanged between adjacent duct
/// cells per sim second (τ ≈ 2 s per pair).
pub const K_DUCT: f32 = 0.5;
/// Fraction of the equalizing amount a vent moves per sim second.
pub const K_VENT: f32 = 0.3;
/// Fraction of the equalizing amount a tank valve moves per sim second.
pub const K_TANK: f32 = 0.25;
/// Maximum gas a blower pushes per sim second (finite throughput).
pub const BLOWER_FLOW: f32 = 12.0;
/// Pressure head a powered blower maintains across its cell pair (kPa). The
/// push stops at the head (no runaway compression in dead ends); the head is
/// what makes exhaust/supply chains actually circulate.
pub const BLOWER_HEAD_KPA: f32 = 15.0;
/// Blower power demand (PU).
pub const BLOWER_DEMAND: u32 = 4;
/// Tank pressure that trips the high-pressure warning (kPa). Transfers are
/// equilibrium-clamped, so this never escalates on its own.
pub const TANK_HIGH_KPA: f32 = 250.0;

/// Steps a duct cell stays awake after its last significant exchange.
pub const WAKE_STEPS: u32 = 600;
/// Moles per pair per step that count as "still interesting".
pub const WAKE_EPS_MOL: f32 = 0.01;

// =====================================================================================
// Duct grid (dense underground layer)
// =====================================================================================

/// Per-tile duct presence + gas state. Not ECS entities; the construction
/// system's transient blueprints/tile entities are the only duct ECS
/// representations, exactly like cables and pipes.
#[derive(Resource)]
pub struct DuctGrid {
    pub width: i32,
    pub height: i32,
    ducts: Vec<bool>,
    /// Gas per duct cell per species (struct-of-arrays).
    pub gas: [Vec<f32>; 4],
    /// Duct gas temperature per cell (the single authority for duct gas
    /// temperature — rooms use `ThermalGrid`, ducts are sealed volumes).
    pub temp: Vec<f32>,
    /// Flow telemetry for the overlay (mol/s, decaying accumulator):
    /// positive X = eastward, positive Y = southward.
    pub flow_x: Vec<f32>,
    pub flow_y: Vec<f32>,
    wake: Vec<u32>,
    awake: Vec<usize>,
    /// Bumped on every `set` (topology rebuild driver).
    pub version: u64,
}

impl DuctGrid {
    pub fn new(width: i32, height: i32) -> Self {
        let n = (width * height) as usize;
        Self {
            width,
            height,
            ducts: vec![false; n],
            gas: [vec![0.0; n], vec![0.0; n], vec![0.0; n], vec![0.0; n]],
            temp: vec![crate::thermal::AMBIENT_START; n],
            flow_x: vec![0.0; n],
            flow_y: vec![0.0; n],
            wake: vec![0; n],
            awake: Vec::new(),
            version: 0,
        }
    }

    pub fn in_bounds(&self, p: TilePos) -> bool {
        p.x >= 0 && p.y >= 0 && p.x < self.width && p.y < self.height
    }

    pub fn has(&self, p: TilePos) -> bool {
        self.in_bounds(p) && self.ducts[self.idx(p)]
    }

    /// Add/remove a duct tile. New ducts boot **empty** (vacuum volume); gas
    /// must physically flow in. Wakes the neighbourhood so the fill starts
    /// this step.
    pub fn set(&mut self, p: TilePos, present: bool) -> bool {
        if !self.in_bounds(p) || self.ducts[self.idx(p)] == present {
            return false;
        }
        let i = self.idx(p);
        self.ducts[i] = present;
        if !present {
            // Removal: the caller drains the gas first (see
            // `remove_duct_preserving_gas`); anything left is gone with the
            // segment, so the caller must account for it.
            for s in 0..4 {
                self.gas[s][i] = 0.0;
            }
            self.flow_x[i] = 0.0;
            self.flow_y[i] = 0.0;
        }
        self.version += 1;
        self.wake_around(p);
        true
    }

    pub fn iter_ducts(&self) -> impl Iterator<Item = TilePos> + '_ {
        let w = self.width;
        self.ducts
            .iter()
            .enumerate()
            .filter(|(_, &d)| d)
            .map(move |(i, _)| TilePos::new(i as i32 % w, i as i32 / w))
    }

    pub fn idx(&self, p: TilePos) -> usize {
        (p.y * self.width + p.x) as usize
    }

    /// Raw duct-presence test by index (transport hardening).
    pub fn is_duct_index(&self, i: usize) -> bool {
        self.ducts.get(i).copied().unwrap_or(false)
    }

    pub fn total_at(&self, i: usize) -> f32 {
        self.gas[0][i] + self.gas[1][i] + self.gas[2][i] + self.gas[3][i]
    }

    pub fn mixture_at(&self, p: TilePos) -> GasMixture {
        let mut m = GasMixture::default();
        if self.has(p) {
            let i = self.idx(p);
            for s in 0..4 {
                m.mol[s] = self.gas[s][i];
            }
        }
        m
    }

    /// Overwrite one cell's gas (in-place transfer helper).
    pub fn set_gas(&mut self, i: usize, mol: &[f32; 4]) {
        for (col, v) in self.gas.iter_mut().zip(mol.iter()) {
            col[i] = v.max(0.0);
        }
    }

    /// Flow telemetry: gas moved `amt` from tile `a` toward tile `b` — both
    /// cells' direction accumulators point along the movement (decaying, so
    /// the overlay shows *current* flow, not history).
    pub fn add_flow_dir(&mut self, a: TilePos, b: TilePos, amt: f32) {
        if !self.in_bounds(a) || !self.in_bounds(b) {
            return;
        }
        let (ia, ib) = (self.idx(a), self.idx(b));
        let fx = (b.x - a.x) as f32 * amt;
        let fy = (b.y - a.y) as f32 * amt;
        self.flow_x[ia] = self.flow_x[ia] * 0.85 + fx;
        self.flow_y[ia] = self.flow_y[ia] * 0.85 + fy;
        self.flow_x[ib] = self.flow_x[ib] * 0.85 + fx;
        self.flow_y[ib] = self.flow_y[ib] * 0.85 + fy;
    }

    /// Derived duct pressure at a tile (kPa) using the duct cell volume.
    pub fn pressure_at(&self, p: TilePos) -> f32 {
        if self.has(p) {
            let i = self.idx(p);
            pressure_vol(self.total_at(i), self.temp[i], DUCT_MOL)
        } else {
            0.0
        }
    }

    // ---- activity ------------------------------------------------------------------

    pub fn wake(&mut self, i: usize) {
        if self.wake[i] == 0 {
            self.awake.push(i);
        }
        self.wake[i] = WAKE_STEPS;
    }

    pub fn wake_at(&mut self, p: TilePos) {
        // Only duct cells participate in transport; waking a non-duct index
        // would let pass 4 "move" gas through a zero-volume phantom cell.
        if self.has(p) {
            self.wake(self.idx(p));
        }
    }

    pub fn wake_around(&mut self, p: TilePos) {
        self.wake_at(p);
        for nb in [
            TilePos::new(p.x + 1, p.y),
            TilePos::new(p.x - 1, p.y),
            TilePos::new(p.x, p.y + 1),
            TilePos::new(p.x, p.y - 1),
        ] {
            self.wake_at(nb);
        }
    }

    pub fn is_awake(&self, i: usize) -> bool {
        self.wake[i] > 0
    }

    fn take_awake(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.awake)
    }

    fn finish_step(&mut self, current: &[usize]) {
        for &i in current {
            self.wake[i] = self.wake[i].saturating_sub(1);
            if self.wake[i] > 0 {
                self.awake.push(i);
            }
        }
    }

    pub fn awake_count(&self) -> usize {
        self.awake.len()
    }

    /// Total gas of one species stored in ducts (mol units).
    pub fn stored(&self, s: usize) -> f64 {
        self.gas[s].iter().map(|&v| v as f64).sum()
    }

    /// Sensible heat stored in duct gas (absolute H) — conservation tests.
    pub fn heat(&self) -> f64 {
        self.gas[0]
            .iter()
            .zip(self.temp.iter())
            .map(|(&n, &t)| (n * GAS_CAP_PER_MOL) as f64 * (t + KELVIN_OFFSET) as f64)
            .sum::<f64>()
            + self.gas[1]
                .iter()
                .zip(self.temp.iter())
                .map(|(&n, &t)| (n * GAS_CAP_PER_MOL) as f64 * (t + KELVIN_OFFSET) as f64)
                .sum::<f64>()
            + self.gas[2]
                .iter()
                .zip(self.temp.iter())
                .map(|(&n, &t)| (n * GAS_CAP_PER_MOL) as f64 * (t + KELVIN_OFFSET) as f64)
                .sum::<f64>()
            + self.gas[3]
                .iter()
                .zip(self.temp.iter())
                .map(|(&n, &t)| (n * GAS_CAP_PER_MOL) as f64 * (t + KELVIN_OFFSET) as f64)
                .sum::<f64>()
    }
}

// =====================================================================================
// Derived topology
// =====================================================================================

/// Per-tile network labelling over the duct layer. Rebuilt only when the
/// duct grid version or the device-set signature changes; gas flow never
/// triggers a rebuild.
#[derive(Resource, Default, Debug, Clone)]
pub struct DuctTopology {
    /// Network id per tile (u16::MAX = no duct).
    pub cell_net: Vec<u16>,
    pub nets: usize,
    pub rebuilds: u32,
    pub version_sig: u64,
    pub device_sig: u64,
}

pub const NO_NET: u16 = u16::MAX;

impl DuctTopology {
    pub fn rebuild(&mut self, ducts: &DuctGrid) {
        self.rebuilds += 1;
        self.version_sig = ducts.version;
        let n = ducts.ducts.len();
        let mut cell_net = vec![NO_NET; n];
        let mut nets = 0usize;
        for y in 0..ducts.height {
            for x in 0..ducts.width {
                let p = TilePos::new(x, y);
                let i = ducts.idx(p);
                if !ducts.ducts[i] || cell_net[i] != NO_NET {
                    continue;
                }
                // Flood fill one 4-connected network (loops and branches are
                // just a visited set).
                let id = nets as u16;
                let mut stack = vec![p];
                cell_net[i] = id;
                while let Some(c) = stack.pop() {
                    for nb in [
                        TilePos::new(c.x + 1, c.y),
                        TilePos::new(c.x - 1, c.y),
                        TilePos::new(c.x, c.y + 1),
                        TilePos::new(c.x, c.y - 1),
                    ] {
                        if ducts.has(nb) {
                            let j = ducts.idx(nb);
                            if cell_net[j] == NO_NET {
                                cell_net[j] = id;
                                stack.push(nb);
                            }
                        }
                    }
                }
                nets += 1;
            }
        }
        self.cell_net = cell_net;
        self.nets = nets;
    }

    pub fn net_at(&self, ducts: &DuctGrid, p: TilePos) -> Option<u16> {
        ducts.has(p).then(|| self.cell_net[ducts.idx(p)])
    }
}

// =====================================================================================
// Devices
// =====================================================================================

/// Vent transfer direction intent.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum VentMode {
    /// Duct → room only.
    #[default]
    Balanced,
    Supply,
    Exhaust,
}

impl VentMode {
    pub fn label(self) -> &'static str {
        match self {
            VentMode::Balanced => "Balanced",
            VentMode::Supply => "Supply",
            VentMode::Exhaust => "Exhaust",
        }
    }

    pub const ALL: [VentMode; 3] = [VentMode::Supply, VentMode::Exhaust, VentMode::Balanced];
}

/// Room atmosphere ↔ duct interface. Exchanges only with the atmosphere cell
/// at its own tile (never a whole compartment).
#[derive(Component, Debug)]
pub struct Vent {
    pub mode: VentMode,
    pub open: bool,
    /// Moles moved in the last step (telemetry for the panel/overlay).
    pub last_rate: f32,
}

impl Default for Vent {
    fn default() -> Self {
        Self {
            mode: VentMode::Balanced,
            open: true,
            last_rate: 0.0,
        }
    }
}

/// Blower push direction (screen space; South = +y).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir4 {
    East,
    West,
    South,
    North,
}

impl Dir4 {
    pub const ALL: [Dir4; 4] = [Dir4::East, Dir4::West, Dir4::South, Dir4::North];

    pub fn label(self) -> &'static str {
        match self {
            Dir4::East => "East",
            Dir4::West => "West",
            Dir4::South => "South",
            Dir4::North => "North",
        }
    }

    pub fn delta(self) -> TilePos {
        match self {
            Dir4::East => TilePos::new(1, 0),
            Dir4::West => TilePos::new(-1, 0),
            Dir4::South => TilePos::new(0, 1),
            Dir4::North => TilePos::new(0, -1),
        }
    }
}

/// Directed duct pusher. A powered blower equalizes pressure *along its
/// direction* at up to `BLOWER_FLOW` per sim second (it never pumps against
/// a higher outlet pressure — deterministic, no runaway in closed loops).
/// Unpowered or disabled: pure passive duct cell (no extra resistance).
#[derive(Component, Debug)]
pub struct Blower {
    pub dir: Dir4,
    pub enabled: bool,
    /// Moles pushed in the last step (telemetry).
    pub last_flow: f32,
}

impl Blower {
    pub fn new(dir: Dir4) -> Self {
        Self {
            dir,
            enabled: true,
            last_flow: 0.0,
        }
    }
}

/// Finite gas storage on a duct tile. Holds a real mixture + temperature;
/// pressure derives through `pressure_vol(.., TANK_MOL)`.
#[derive(Component, Debug, Clone, Copy)]
pub struct GasTank {
    pub mix: GasMixture,
    pub temp: f32,
    pub valve_open: bool,
}

impl Default for GasTank {
    fn default() -> Self {
        Self {
            mix: GasMixture::default(),
            temp: crate::thermal::AMBIENT_START,
            valve_open: true,
        }
    }
}

impl GasTank {
    /// Starter tank: standard atmosphere fill at tank volume.
    pub fn prefilled_standard() -> Self {
        let mut mix = GasMixture::default();
        for (s, &v) in crate::atmosphere::STANDARD_MIX.iter().enumerate() {
            mix.mol[s] = v * TANK_MOL / crate::atmosphere::STANDARD_MOL;
        }
        Self {
            mix,
            temp: crate::thermal::AMBIENT_START,
            valve_open: true,
        }
    }

    pub fn total(&self) -> f32 {
        self.mix.total()
    }

    pub fn pressure(&self) -> f32 {
        pressure_vol(self.total(), self.temp, TANK_MOL)
    }

    /// Sensible heat of the stored gas (absolute H).
    pub fn heat(&self) -> f64 {
        (self.total() * GAS_CAP_PER_MOL) as f64 * (self.temp + KELVIN_OFFSET) as f64
    }
}

/// Infer a blower's initial direction from the surrounding duct run (the
/// long axis wins; ambiguous spots default to East — the panel can change
/// it at any time).
pub fn infer_blower_dir(ducts: &DuctGrid, p: TilePos) -> Dir4 {
    let e = ducts.has(TilePos::new(p.x + 1, p.y));
    let w = ducts.has(TilePos::new(p.x - 1, p.y));
    let s = ducts.has(TilePos::new(p.x, p.y + 1));
    let n = ducts.has(TilePos::new(p.x, p.y - 1));
    if e && !w {
        Dir4::East
    } else if w && !e {
        Dir4::West
    } else if s && !n {
        Dir4::South
    } else if n && !s {
        Dir4::North
    } else if e || w {
        Dir4::East
    } else {
        Dir4::North
    }
}

// =====================================================================================
// Ledger + summary
// =====================================================================================

/// Ventilation accounting + telemetry.
#[derive(Resource, Default, Debug, Clone)]
pub struct VentStats {
    /// Gas lost to space by duct/tank removal with nowhere to go, per
    /// species (the only permitted sink; everything else is bookkept by the
    /// atmosphere's own ledger once released into a room).
    pub vented_mol: [f64; 4],
    pub vented_energy: f64,
    /// Awake duct cells in the last step.
    pub active_cells: usize,
    /// Duct↔duct edge updates in the last step (perf telemetry).
    pub edge_updates: usize,
    pub steps: u64,
}

/// Cached UI summary, recomputed on a sim-step cadence.
#[derive(Resource, Default, Debug, Clone)]
pub struct VentSummary {
    pub networks: usize,
    pub active_cells: usize,
    /// Total gas in ducts + tanks (mol units).
    pub stored_mol: f64,
    /// (powered, total) blowers that are enabled.
    pub blowers_on: u32,
    pub blowers_total: u32,
    /// Highest tank pressure (kPa).
    pub max_tank_p: f32,
    pub no_duct_vents: u32,
    pub unpowered_blowers: u32,
}

impl VentSummary {
    pub fn alert(&self) -> Option<&'static str> {
        if self.unpowered_blowers > 0 {
            Some("BLOWER NO POWER")
        } else if self.max_tank_p > TANK_HIGH_KPA {
            Some("TANK HIGH PRESSURE")
        } else if self.no_duct_vents > 0 {
            Some("VENT NO DUCT")
        } else {
            None
        }
    }
}

// =====================================================================================
// Structural gas handling
// =====================================================================================

/// Remove a duct tile conserving its gas: first push an even share into the
/// still-connected duct neighbours, release the remainder into the local
/// room atmosphere (energy included), and only if the tile has no interior
/// air volume book it as vented to space in the ventilation ledger.
pub fn remove_duct_preserving_gas(
    map: &ShipMap,
    ducts: &mut DuctGrid,
    atmo: &mut crate::atmosphere::AtmosphereGrid,
    thermal: &mut crate::thermal::ThermalGrid,
    vstats: &mut VentStats,
    p: TilePos,
) {
    if !ducts.has(p) {
        return;
    }
    let i = ducts.idx(p);
    let mut mix = ducts.mixture_at(p);
    let temp = ducts.temp[i];
    // 1. Still-connected duct neighbours take an even share (pressure rises
    // a little — a real, conserved squeeze).
    let mut nbs: Vec<usize> = [
        TilePos::new(p.x + 1, p.y),
        TilePos::new(p.x - 1, p.y),
        TilePos::new(p.x, p.y + 1),
        TilePos::new(p.x, p.y - 1),
    ]
    .iter()
    .filter(|&&n| ducts.has(n))
    .map(|&n| ducts.idx(n))
    .collect();
    nbs.sort_unstable();
    nbs.dedup();
    if !nbs.is_empty() {
        let share = 1.0 / nbs.len() as f32;
        for &j in &nbs {
            let mut part_sum = 0.0f32;
            for s in 0..4 {
                let part = mix.mol[s] * share;
                mix.mol[s] -= part;
                ducts.gas[s][j] += part;
                part_sum += part;
            }
            // Energy-conserving temperature mix into the neighbour.
            let n_old = ducts.total_at(j) - part_sum;
            let q = part_sum * GAS_CAP_PER_MOL * (temp + KELVIN_OFFSET);
            let e_old = n_old * GAS_CAP_PER_MOL * (ducts.temp[j] + KELVIN_OFFSET);
            ducts.temp[j] = ((e_old + q) / (ducts.total_at(j) * GAS_CAP_PER_MOL)) - KELVIN_OFFSET;
            ducts.wake(j);
        }
    }
    // 2. Remainder → local room atmosphere, or the vent ledger.
    let rest: f32 = mix.mol.iter().sum();
    if rest > 1e-4 {
        if crate::atmosphere::is_air_tile(map.tile(p)) {
            let q = (rest * GAS_CAP_PER_MOL) as f64 * (temp + KELVIN_OFFSET) as f64;
            let cap_old = thermal.gas_cap[i];
            let e_old = cap_old as f64 * (thermal.amb[i] + KELVIN_OFFSET) as f64;
            atmo.inject(p, &mix);
            thermal.gas_cap[i] += rest * GAS_CAP_PER_MOL;
            thermal.amb[i] = ((e_old + q) / thermal.gas_cap[i] as f64) as f32 - KELVIN_OFFSET;
            thermal.wake(i);
            atmo.wake_at(p);
        } else {
            for s in 0..4 {
                vstats.vented_mol[s] += mix.mol[s] as f64;
            }
            vstats.vented_energy += (rest * GAS_CAP_PER_MOL) as f64 * (temp + KELVIN_OFFSET) as f64;
        }
    }
    ducts.set(p, false);
}

/// Release a tank's gas when it is torn down: into the duct below (if any),
/// then the local room atmosphere; gas with nowhere to go is ledgered as
/// vented to space. Species and thermal energy are conserved throughout.
pub fn release_tank_gas(
    map: &ShipMap,
    ducts: Option<&mut DuctGrid>,
    atmo: Option<&mut crate::atmosphere::AtmosphereGrid>,
    thermal: &mut crate::thermal::ThermalGrid,
    vstats: Option<&mut VentStats>,
    tank: GasTank,
    p: TilePos,
) {
    let cppm = GAS_CAP_PER_MOL;
    let koff = KELVIN_OFFSET;
    let mut mix = tank.mix;
    if let Some(ducts) = ducts {
        if ducts.has(p) {
            let i = ducts.idx(p);
            for s in 0..4 {
                ducts.gas[s][i] += mix.mol[s];
            }
            let rest = mix.total();
            let q = rest * cppm * (tank.temp + koff);
            let e_old = (ducts.total_at(i) - rest) * cppm * (ducts.temp[i] + koff);
            ducts.temp[i] = ((e_old + q) / (ducts.total_at(i) * cppm)) - koff;
            ducts.wake(i);
            mix = GasMixture::default();
        }
    }
    let rest = mix.total();
    if rest > 1e-4 {
        let p_i = (p.y * map.width + p.x) as usize;
        if crate::atmosphere::is_air_tile(map.tile(p)) {
            if let Some(atmo) = atmo {
                let q = (rest * cppm) as f64 * (tank.temp + koff) as f64;
                let e_old = thermal.gas_cap[p_i] as f64 * (thermal.amb[p_i] + koff) as f64;
                atmo.inject(p, &mix);
                thermal.gas_cap[p_i] += rest * cppm;
                thermal.amb[p_i] = ((e_old + q) / thermal.gas_cap[p_i] as f64) as f32 - koff;
                thermal.wake(p_i);
            }
        } else if let Some(vstats) = vstats {
            for s in 0..4 {
                vstats.vented_mol[s] += mix.mol[s] as f64;
            }
            vstats.vented_energy += (rest * cppm) as f64 * (tank.temp + koff) as f64;
        }
    }
}

// =====================================================================================
// System
// =====================================================================================

pub struct VentilationPlugin;

impl Plugin for VentilationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VentStats>();
        app.init_resource::<VentSummary>();
        app.add_systems(
            FixedUpdate,
            // Single authority for vent/blower/tank transfers. Runs after the
            // atmosphere step (fresh room state; its room-side writes land
            // next atmosphere step) and after the power pass that decided
            // this step's blower supply.
            ventilation_system
                .after(crate::atmosphere::atmosphere_system)
                .before(crate::Set::Move)
                .in_set(crate::Set::Jobs),
        );
        app.add_systems(Update, vent_action_system.in_set(crate::Set::Input));
    }
}

/// Advance the ventilation network one sim step: vents, blowers and tanks
/// (device passes — a handful of entities, always processed), then duct↔duct
/// transport over awake cells only. `dt = 0` (pause) freezes everything.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
pub fn ventilation_system(
    clock: Res<crate::simtime::SimClock>,
    mut atmo: ResMut<crate::atmosphere::AtmosphereGrid>,
    mut thermal: ResMut<crate::thermal::ThermalGrid>,
    devices: Res<crate::thermal::DeviceTiles>,
    mut ducts: ResMut<DuctGrid>,
    mut topo: ResMut<DuctTopology>,
    mut vstats: ResMut<VentStats>,
    mut vsum: ResMut<VentSummary>,
    mut vents: Query<(&TilePos, &mut Vent)>,
    mut blowers: Query<(&TilePos, &mut Blower, &crate::power::PowerStatus)>,
    mut tanks: Query<(&TilePos, &mut GasTank)>,
) {
    let dt = clock.dt() as f32;
    // Topology: rebuilt on duct edits or device-set changes — never by flow.
    let device_sig = (vents.iter().count() as u64)
        .wrapping_mul(1_000_003)
        .wrapping_add(blowers.iter().count() as u64)
        .wrapping_mul(1_000_033)
        .wrapping_add(tanks.iter().count() as u64);
    if topo.version_sig != ducts.version || topo.device_sig != device_sig {
        topo.device_sig = device_sig;
        topo.rebuild(&ducts);
    }
    if dt <= 0.0 {
        vstats.steps += 1;
        return;
    }

    // ---- 1. vents (room ↔ duct at the vent's own tile) -----------------------------
    for (pos, mut vent) in vents.iter_mut() {
        vent.last_rate = 0.0;
        if !vent.open || !ducts.has(*pos) {
            continue;
        }
        let i = ducts.idx(*pos);
        let room_i = atmo.idx(*pos);
        let n_room = atmo.total_at(room_i);
        let n_duct = ducts.total_at(i);
        let x = eq_amount(
            n_room,
            thermal.amb[room_i],
            crate::atmosphere::STANDARD_MOL,
            n_duct,
            ducts.temp[i],
            DUCT_MOL,
        );
        let (amount, room_to_duct) = match vent.mode {
            VentMode::Balanced => (x.abs(), x > 0.0),
            VentMode::Exhaust => (x.max(0.0), true),
            VentMode::Supply => (x.min(0.0).abs(), false),
        };
        let amount = amount * (K_VENT * dt).min(1.0);
        // Noise floor == a fraction of the wake epsilon: sub-epsilon
        // trickles are skipped so an equalized network can sleep.
        if amount <= WAKE_EPS_MOL * 0.1 {
            continue;
        }
        let extra = devices.mass_at(room_i);
        if room_to_duct {
            let mut src = atmo.mixture_at(*pos).mol;
            let mut dst = ducts.mixture_at(*pos).mol;
            let mut dtemp = ducts.temp[i];
            move_gas(
                &mut src,
                &mut dst,
                thermal.amb[room_i],
                &mut dtemp,
                0.0,
                amount,
            );
            ducts.set_gas(i, &dst);
            ducts.temp[i] = dtemp;
            atmo.set_mixture(*pos, GasMixture { mol: src });
            thermal.gas_cap[room_i] = (atmo.total_at(room_i) * GAS_CAP_PER_MOL).max(0.0);
        } else {
            let mut src = ducts.mixture_at(*pos).mol;
            let mut room_mix = atmo.mixture_at(*pos).mol;
            let mut rtemp = thermal.amb[room_i];
            move_gas(
                &mut src,
                &mut room_mix,
                ducts.temp[i],
                &mut rtemp,
                extra,
                amount,
            );
            ducts.set_gas(i, &src);
            atmo.set_mixture(*pos, GasMixture { mol: room_mix });
            thermal.amb[room_i] = rtemp;
            thermal.gas_cap[room_i] = (atmo.total_at(room_i) * GAS_CAP_PER_MOL).max(0.0);
        }
        vent.last_rate = amount;
        ducts.wake(i);
        atmo.wake_at(*pos);
        thermal.wake(room_i);
    }

    // ---- 2. blowers (directed push along the duct) ----------------------------------
    for (pos, mut blower, power) in blowers.iter_mut() {
        blower.last_flow = 0.0;
        if !blower.enabled || !power.ok() || !ducts.has(*pos) {
            continue;
        }
        let d = blower.dir.delta();
        let out = TilePos::new(pos.x + d.x, pos.y + d.y);
        if !ducts.has(out) {
            continue;
        }
        let (i, j) = (ducts.idx(*pos), ducts.idx(out));
        // Push up to the flow cap, but only until the outlet sits HEAD above
        // the inlet (closed form of ((n_o+x)·T_o − (n_i−x)·T_i) · P_ref /
        // (V·T_ref) = HEAD). Equal-pressure cells push ~0.7 mol/step, a
        // dead-end stalls at +HEAD, a loop circulates forever at the cap.
        let (n_i, n_o) = (ducts.total_at(i), ducts.total_at(j));
        let ti = ducts.temp[i] + KELVIN_OFFSET;
        let to = ducts.temp[j] + KELVIN_OFFSET;
        let k = BLOWER_HEAD_KPA / PRESSURE_REF * DUCT_MOL * TEMP_REF;
        let x = (k + n_i * ti - n_o * to) / (ti + to);
        let amount = x.max(0.0).min(BLOWER_FLOW * dt);
        if amount <= WAKE_EPS_MOL * 0.1 {
            continue;
        }
        let mut src = ducts.mixture_at(*pos).mol;
        let mut dst = ducts.mixture_at(out).mol;
        let mut dtemp = ducts.temp[j];
        move_gas(&mut src, &mut dst, ducts.temp[i], &mut dtemp, 0.0, amount);
        ducts.set_gas(i, &src);
        ducts.set_gas(j, &dst);
        ducts.temp[j] = dtemp;
        blower.last_flow = amount;
        ducts.wake(i);
        ducts.wake(j);
        ducts.add_flow_dir(*pos, out, amount);
    }

    // ---- 3. tanks (valve-gated pressure exchange with the duct below) ---------------
    for (pos, mut tank) in tanks.iter_mut() {
        if !tank.valve_open || !ducts.has(*pos) {
            continue;
        }
        let i = ducts.idx(*pos);
        let x = eq_amount(
            tank.total(),
            tank.temp,
            TANK_MOL,
            ducts.total_at(i),
            ducts.temp[i],
            DUCT_MOL,
        );
        let amount = x * (K_TANK * dt).min(1.0);
        if amount.abs() <= WAKE_EPS_MOL * 0.1 {
            continue;
        }
        if amount > 0.0 {
            let mut src = tank.mix.mol;
            let mut dst = ducts.mixture_at(*pos).mol;
            let mut dtemp = ducts.temp[i];
            move_gas(&mut src, &mut dst, tank.temp, &mut dtemp, 0.0, amount);
            tank.mix.mol = src;
            ducts.set_gas(i, &dst);
            ducts.temp[i] = dtemp;
        } else {
            let mut src = ducts.mixture_at(*pos).mol;
            let mut dst = tank.mix.mol;
            let mut ttemp = tank.temp;
            move_gas(&mut src, &mut dst, ducts.temp[i], &mut ttemp, 0.0, -amount);
            ducts.set_gas(i, &src);
            tank.mix.mol = dst;
            tank.temp = ttemp;
        }
        ducts.wake(i);
    }

    // ---- 4. duct ↔ duct transport over awake cells ----------------------------------
    let current = ducts.take_awake();
    vstats.active_cells = current.len();
    let mut edges = 0usize;
    for &i in &current {
        if !ducts.is_duct_index(i) {
            continue;
        }
        let p = ducts_pos(ducts.width, i);
        for nb in [
            TilePos::new(p.x, p.y - 1),
            TilePos::new(p.x, p.y + 1),
            TilePos::new(p.x - 1, p.y),
            TilePos::new(p.x + 1, p.y),
        ] {
            if !ducts.has(nb) {
                continue;
            }
            let j = ducts.idx(nb);
            if ducts.is_awake(j) && j < i {
                continue;
            }
            edges += 1;
            let x = eq_amount(
                ducts.total_at(i),
                ducts.temp[i],
                DUCT_MOL,
                ducts.total_at(j),
                ducts.temp[j],
                DUCT_MOL,
            );
            let amount = x * (K_DUCT * dt).min(1.0);
            if amount.abs() <= WAKE_EPS_MOL * 0.01 {
                continue;
            }
            let (a, b, amt) = if amount > 0.0 {
                (p, nb, amount)
            } else {
                (nb, p, -amount)
            };
            let (ia, ib) = (ducts.idx(a), ducts.idx(b));
            let mut src = ducts.mixture_at(a).mol;
            let mut dst = ducts.mixture_at(b).mol;
            let mut dtemp = ducts.temp[ib];
            move_gas(&mut src, &mut dst, ducts.temp[ia], &mut dtemp, 0.0, amt);
            ducts.set_gas(ia, &src);
            ducts.set_gas(ib, &dst);
            ducts.temp[ib] = dtemp;
            ducts.add_flow_dir(a, b, amt);
            if amt > WAKE_EPS_MOL {
                ducts.wake(j);
            }
        }
    }
    ducts.finish_step(&current);
    vstats.edge_updates = edges;
    vstats.steps += 1;

    // ---- 5. cached UI summary on a cadence -------------------------------------------
    if vstats.steps % 30 == 1 || vsum.networks == 0 {
        *vsum = summarize(&ducts, &topo, &vstats, &vents, &blowers, &tanks);
    }
}

fn ducts_pos(width: i32, i: usize) -> TilePos {
    TilePos::new(i as i32 % width, i as i32 / width)
}

/// Cached summary scan (cadence, never per frame).
#[allow(clippy::type_complexity)]
fn summarize(
    ducts: &DuctGrid,
    topo: &DuctTopology,
    vstats: &VentStats,
    vents: &Query<(&TilePos, &mut Vent)>,
    blowers: &Query<(&TilePos, &mut Blower, &crate::power::PowerStatus)>,
    tanks: &Query<(&TilePos, &mut GasTank)>,
) -> VentSummary {
    let mut s = VentSummary {
        networks: topo.nets,
        active_cells: vstats.active_cells,
        ..default()
    };
    for s_i in 0..4 {
        s.stored_mol += ducts.stored(s_i);
    }
    for (pos, _vent) in vents.iter() {
        if !ducts.has(*pos) {
            s.no_duct_vents += 1;
        }
    }
    for (_, blower, power) in blowers.iter() {
        if blower.enabled {
            s.blowers_total += 1;
            if power.ok() {
                s.blowers_on += 1;
            } else {
                s.unpowered_blowers += 1;
            }
        }
    }
    for (_, tank) in tanks.iter() {
        s.stored_mol += tank.total() as f64;
        s.max_tank_p = s.max_tank_p.max(tank.pressure());
    }
    s
}

/// Player actions on ventilation devices (frame-based, like door modes).
pub fn vent_action_system(
    mut events: EventReader<crate::jobs::Action>,
    mut vents: Query<(Entity, &TilePos, &mut Vent)>,
    mut blowers: Query<(Entity, &TilePos, &mut Blower)>,
    mut tanks: Query<(Entity, &TilePos, &mut GasTank)>,
    mut ducts: ResMut<DuctGrid>,
    mut log: ResMut<crate::log::EventLog>,
    clock: Res<crate::simtime::SimClock>,
) {
    let now = clock.now();
    for action in events.read() {
        match *action {
            crate::jobs::Action::SetVentMode { vent, mode } => {
                if let Ok((_, pos, mut v)) = vents.get_mut(vent) {
                    v.mode = mode;
                    ducts.wake_at(*pos);
                    log.push(
                        now,
                        crate::log::LogKind::Info,
                        format!("Vent at ({},{}) -> {}", pos.x, pos.y, mode.label()),
                    );
                }
            }
            crate::jobs::Action::SetVentOpen { vent, open } => {
                if let Ok((_, pos, mut v)) = vents.get_mut(vent) {
                    v.open = open;
                    ducts.wake_at(*pos);
                    log.push(
                        now,
                        crate::log::LogKind::Info,
                        format!(
                            "Vent at ({},{}) {}",
                            pos.x,
                            pos.y,
                            if open { "opened" } else { "closed" }
                        ),
                    );
                }
            }
            crate::jobs::Action::SetBlowerDir { blower, dir } => {
                if let Ok((_, pos, mut b)) = blowers.get_mut(blower) {
                    b.dir = dir;
                    ducts.wake_at(*pos);
                    log.push(
                        now,
                        crate::log::LogKind::Info,
                        format!("Blower at ({},{}) -> {}", pos.x, pos.y, dir.label()),
                    );
                }
            }
            crate::jobs::Action::SetBlowerOn { blower, on } => {
                if let Ok((_, pos, mut b)) = blowers.get_mut(blower) {
                    b.enabled = on;
                    ducts.wake_at(*pos);
                    log.push(
                        now,
                        crate::log::LogKind::Info,
                        format!(
                            "Blower at ({},{}) {}",
                            pos.x,
                            pos.y,
                            if on { "on" } else { "off" }
                        ),
                    );
                }
            }
            crate::jobs::Action::SetTankValve { tank, open } => {
                if let Ok((_, pos, mut t)) = tanks.get_mut(tank) {
                    t.valve_open = open;
                    ducts.wake_at(*pos);
                    log.push(
                        now,
                        crate::log::LogKind::Info,
                        format!(
                            "Tank at ({},{}) valve {}",
                            pos.x,
                            pos.y,
                            if open { "opened" } else { "closed" }
                        ),
                    );
                }
            }
            _ => {}
        }
    }
}
