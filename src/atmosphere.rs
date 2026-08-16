//! Atmosphere & pressure (Slice 5): air as a real, per-cell, conserved,
//! flowable resource.
//!
//! The authoritative state is a dense per-tile grid of **gas amounts** —
//! never one ECS entity per cell, never a room-average. Pressure is strictly
//! derived (`P ∝ n·T` ideal-gas-like with a uniform effective cell volume);
//! so is every partial pressure and percentage shown in the UI.
//!
//! Boundary semantics are *reused* from `airtight::boundary` — walls block,
//! closed/opening/closing doors block, fully open doors (and floor/machine
//! tiles) exchange. Door tiles are modelled as gas volume (they carry a real
//! standard fill that sits trapped while the door is sealed and exchanges
//! with both sides while open); this matches the thermal model, where the
//! door tile is an ordinary air node, and makes opening/closing inherently
//! gas-neutral (no ghost volume to create or destroy).
//!
//! Transport per sim step, 4-orthogonal neighbours, each unordered pair once:
//! 1. **Vent** — gas cells 4-adjacent to the map exterior leak to vacuum
//!    (zero pressure, infinite sink, no exterior grid). Venting gas carries
//!    its sensible heat out of the ship and is ledgered per species.
//! 2. **Bulk flow** — pressure difference moves whole mixture (species
//!    proportionally) with an equilibrium clamp: the flux can never exceed
//!    the amount that exactly equalizes the pair, so no overshoot for any dt.
//! 3. **Advection** — bulk flow moves `δ·cppm·T` of sensible heat; the
//!    source keeps its temperature (removing gas does not cool it), the
//!    destination mixes energies over its new capacity.
//! 4. **Diffusion** — slow composition mixing that equalizes mole fractions
//!    at equal total pressure; per species conservative, no thermal effect.
//!
//! Activity model mirrors `thermal.rs`: cells wake on door seal flips,
//! structural edits, injections, thermal change at their tile, or a
//! significant neighbour exchange, and sleep after `WAKE_STEPS` quiet steps.
//! A uniform sealed ship costs ~zero per step.

use crate::map::{ShipMap, Tile, TilePos};
use bevy::prelude::*;

// =====================================================================================
// Units and tunables
// =====================================================================================

/// Reference pressure of one standard atmosphere (kPa).
pub const PRESSURE_REF: f32 = 101.325;
/// Reference absolute temperature of the standard atmosphere (K): 21 °C,
/// matching the thermal boot ambient.
pub const TEMP_REF: f32 = 294.15;
/// Gas amount (mol-equivalent units) that fills one cell at the standard
/// atmosphere. The "effective cell volume" is defined by this number: with
/// the uniform-volume ideal-gas relation below, 100 units at 294.15 K are
/// exactly 101.325 kPa. Internal units are deliberately normalized mol
/// (report in `REPORT_ATMOSPHERE.md`); the player only ever sees kPa / %.
pub const STANDARD_MOL: f32 = 100.0;
/// Standard boot composition per cell: 21 O₂ / 78.6 inert / 0.4 CO₂ / 0
/// pollutant — ~21.3 kPa O₂ partial at 101.3 kPa total, low CO₂.
pub const STANDARD_MIX: [f32; 4] = [21.0, 78.6, 0.4, 0.0];

/// °C zero point for absolute accounting (same constant as the thermal grid).
pub const KELVIN_OFFSET: f32 = 273.15;

/// Gas heat capacity per unit (H/K per mol-unit). 100 units (standard fill)
/// reproduce the historical fixed air capacity of 24 H/K, so thermal balance
/// is unchanged while pressurized; a vacuum tile's gas capacity is ~0.
pub const GAS_CAP_PER_MOL: f32 = 0.24;

/// Fraction of the pair-equalizing amount moved per sim second by bulk flow.
/// τ ≈ 8 sim s per cell pair: pressure fronts propagate cell-by-cell, a
/// door equalization between rooms takes visible seconds, a breach empties
/// its room over a fraction of a ship-minute.
pub const K_BULK: f32 = 0.12;
/// Fraction of the composition-equalizing amount mixed per sim second.
/// Deliberately much slower than bulk flow (τ ≈ 50 s per pair).
pub const K_DIFF: f32 = 0.02;
/// Fraction of an exposed cell's gas vented to space per sim second
/// (τ ≈ 2.5 s per exposed cell; neighbours feed it via bulk flow).
pub const K_VENT: f32 = 0.4;

/// Steps a gas cell stays awake after its last significant exchange.
pub const WAKE_STEPS: u32 = 600;
/// Moles moved per pair per step that count as "still interesting".
pub const WAKE_EPS_MOL: f32 = 0.01;
/// Temperature change per step (K) that is pressure-relevant for the gas
/// side of the thermal→atmosphere wake hook. Much coarser than the thermal
/// wake epsilon: a ship-wide slow drift toward thermal equilibrium shifts
/// pressure by orders of magnitude less than any real flow, so the gas cells
/// sleep through it instead of mirroring every thermal wake.
pub const THERMAL_WAKE_EPS: f32 = 0.05;
/// Below this pressure a cell reads as vacuum in the UI (kPa).
pub const VACUUM_KPA: f32 = 0.5;
/// Total gas below which a pair is treated as empty (mol units).
const EPS_MOL: f32 = 1e-4;

/// Derive total pressure (kPa) from gas amount and temperature (°C).
/// Ideal-gas-like with uniform effective volume: P = P_ref · (n/n_ref) · (T/T_ref).
#[inline]
pub fn pressure(total_mol: f32, temp_c: f32) -> f32 {
    pressure_vol(total_mol, temp_c, STANDARD_MOL)
}

/// Pressure (kPa) of a gas amount in an arbitrary finite volume expressed in
/// mol-units-at-standard (`volume_mol` = how many units would fill this
/// container to the reference atmosphere). Ducts and tanks use this with
/// their own volumes — same semantics, different container.
#[inline]
pub fn pressure_vol(total_mol: f32, temp_c: f32, volume_mol: f32) -> f32 {
    PRESSURE_REF * (total_mol / volume_mol) * ((temp_c + KELVIN_OFFSET) / TEMP_REF)
}

/// Amount (mol) that exactly equalizes the *pressures* of two finite
/// containers (positive = a → b), given amounts, temperatures and volumes.
/// Derived from `(n_a − x)·T_a/V_a = (n_b + x)·T_b/V_b`.
#[inline]
pub fn eq_amount(n_a: f32, t_a: f32, vol_a: f32, n_b: f32, t_b: f32, vol_b: f32) -> f32 {
    let ta = t_a + KELVIN_OFFSET;
    let tb = t_b + KELVIN_OFFSET;
    (n_a * ta / vol_a - n_b * tb / vol_b) / (ta / vol_a + tb / vol_b)
}

/// The single transfer primitive shared by atmosphere bulk flow and every
/// ventilation move (vents, blowers, tanks): move `amount` mol of gas from
/// `src` to `dst`, species proportionally to the source composition. The
/// source keeps its temperature (removing gas does not cool it); the
/// destination mixes energies over its new gas capacity plus any extra
/// (device) mass sharing the node. Clamped non-negative, no-op on empty
/// sources.
pub fn move_gas(
    src: &mut [f32; 4],
    dst: &mut [f32; 4],
    src_temp_c: f32,
    dst_temp_c: &mut f32,
    dst_extra_cap: f32,
    amount: f32,
) {
    if amount <= 0.0 {
        return;
    }
    let n_src: f32 = src.iter().sum();
    if n_src <= EPS_MOL {
        return;
    }
    let a = amount.min(n_src);
    for s in 0..4 {
        let mv = a * (src[s] / n_src);
        src[s] -= mv;
        dst[s] += mv;
    }
    let q = a * GAS_CAP_PER_MOL * (src_temp_c + KELVIN_OFFSET);
    let n_dst_new: f32 = dst.iter().sum();
    let cap_dst = n_dst_new * GAS_CAP_PER_MOL + dst_extra_cap;
    if cap_dst > 0.0 {
        let e_dst = (n_dst_new - a) * GAS_CAP_PER_MOL * (*dst_temp_c + KELVIN_OFFSET)
            + dst_extra_cap * (*dst_temp_c + KELVIN_OFFSET);
        *dst_temp_c = ((e_dst + q) / cap_dst) - KELVIN_OFFSET;
    }
}

/// Tiles that carry room air volume (floor / machine / door). Public for the
/// ventilation layer's release rules.
pub fn is_air_tile(t: Option<crate::map::Tile>) -> bool {
    matches!(
        t,
        Some(crate::map::Tile::Floor)
            | Some(crate::map::Tile::Machine)
            | Some(crate::map::Tile::Door)
    )
}

/// Partial pressure of one species (kPa) from its amount, the cell total and
/// the temperature. Equivalent to `P_total × fraction`, but stable at vacuum.
#[inline]
pub fn partial_pressure(amount: f32, total_mol: f32, temp_c: f32) -> f32 {
    if total_mol <= EPS_MOL {
        0.0
    } else {
        pressure(total_mol, temp_c) * (amount / total_mol)
    }
}

// =====================================================================================
// Species
// =====================================================================================

/// The four fixed gas species of the first version. Dense index into every
/// per-species array — no `HashMap<ChemicalId, _>` anywhere.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Species {
    O2 = 0,
    Inert = 1,
    Co2 = 2,
    Pollutant = 3,
}

pub const SPECIES: [Species; 4] = [
    Species::O2,
    Species::Inert,
    Species::Co2,
    Species::Pollutant,
];

impl Species {
    pub fn label(self) -> &'static str {
        match self {
            Species::O2 => "O2",
            Species::Inert => "inert",
            Species::Co2 => "CO2",
            Species::Pollutant => "pollutant",
        }
    }
}

/// A per-cell gas snapshot (mol units per species).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GasMixture {
    pub mol: [f32; 4],
}

impl GasMixture {
    pub fn total(&self) -> f32 {
        self.mol.iter().sum()
    }

    pub fn standard() -> Self {
        Self { mol: STANDARD_MIX }
    }

    /// Mole fraction of one species (0 when the cell is empty).
    pub fn fraction(&self, s: Species) -> f32 {
        let t = self.total();
        if t <= EPS_MOL {
            0.0
        } else {
            self.mol[s as usize] / t
        }
    }
}

// =====================================================================================
// Grid
// =====================================================================================

/// Dense per-tile atmosphere state: four species arrays plus wake/sleep
/// bookkeeping and the exterior-vent work list. Gas cells are floor, machine
/// and door tiles; wall tiles store no gas (their arrays simply hold zeros).
#[derive(Resource)]
pub struct AtmosphereGrid {
    pub width: i32,
    pub height: i32,
    /// Gas amount per tile per species (struct-of-arrays for the inner loop).
    pub gas: [Vec<f32>; 4],
    /// Gas cells that are 4-adjacent to the map exterior (recomputed on
    /// structural edits only).
    exposed: Vec<usize>,
    wake: Vec<u32>,
    /// Worklist of awake cell indices (exactly those with `wake > 0`).
    awake: Vec<usize>,
    /// `ShipMap::version` the exposed list was built from.
    geometry_version: u64,
}

/// Tiles that carry gas volume. Door tiles included: they are portals in the
/// compartment graph but real (small) air volume here and in the thermal
/// model — that is the Slice 5 "door-cell gas volume" choice.
fn is_gas_tile(t: Option<Tile>) -> bool {
    matches!(
        t,
        Some(Tile::Floor) | Some(Tile::Machine) | Some(Tile::Door)
    )
}

impl AtmosphereGrid {
    pub fn new(map: &ShipMap) -> Self {
        let n = (map.width * map.height) as usize;
        let mut me = Self {
            width: map.width,
            height: map.height,
            gas: [
                vec![STANDARD_MIX[0]; n],
                vec![STANDARD_MIX[1]; n],
                vec![STANDARD_MIX[2]; n],
                vec![STANDARD_MIX[3]; n],
            ],
            exposed: Vec::new(),
            wake: vec![0; n],
            awake: Vec::new(),
            geometry_version: u64::MAX,
        };
        // Walls hold no gas: zero their arrays. (Doors/machines keep the
        // standard fill — they are gas cells.)
        for (p, tile) in map.iter_tiles() {
            if !is_gas_tile(Some(tile)) {
                let i = me.idx(p);
                for s in SPECIES {
                    me.gas[s as usize][i] = 0.0;
                }
            }
        }
        me.refresh_geometry(map);
        me
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

    /// Gas snapshot at a tile (zeros for walls / out of bounds).
    pub fn mixture_at(&self, p: TilePos) -> GasMixture {
        let mut m = GasMixture::default();
        if self.in_bounds(p) {
            let i = self.idx(p);
            for s in SPECIES {
                m.mol[s as usize] = self.gas[s as usize][i];
            }
        }
        m
    }

    pub fn total_at(&self, i: usize) -> f32 {
        self.gas[0][i] + self.gas[1][i] + self.gas[2][i] + self.gas[3][i]
    }

    /// Pressure at a tile (kPa) using the authoritative thermal temperature.
    pub fn pressure_at(&self, p: TilePos, thermal: &crate::thermal::ThermalGrid) -> f32 {
        if !self.in_bounds(p) {
            return 0.0;
        }
        let i = self.idx(p);
        pressure(self.total_at(i), thermal.amb[i])
    }

    // ---- activity ------------------------------------------------------------------

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

    /// Wake a tile and its 4 neighbours (door flips, breaches, injections).
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

    /// Recompute the exterior-vent list after a structural change and wake
    /// the affected cells so exposure begins (or ends) this step.
    pub fn refresh_geometry(&mut self, map: &ShipMap) {
        self.geometry_version = map.version;
        let mut exposed = Vec::new();
        for (p, tile) in map.iter_tiles() {
            if !is_gas_tile(Some(tile)) {
                continue;
            }
            let touches_space = [
                TilePos::new(p.x + 1, p.y),
                TilePos::new(p.x - 1, p.y),
                TilePos::new(p.x, p.y + 1),
                TilePos::new(p.x, p.y - 1),
            ]
            .iter()
            .any(|&nb| !map.in_bounds(nb));
            if touches_space {
                exposed.push(self.idx(p));
            }
        }
        for &i in &exposed {
            self.wake(i);
        }
        self.exposed = exposed;
    }

    // ---- mutations (debug / scenario / structural) ---------------------------------

    /// Add gas at a tile (wakes it). Negative amounts are ignored.
    pub fn inject(&mut self, p: TilePos, mix: &GasMixture) {
        if !self.in_bounds(p) {
            return;
        }
        let i = self.idx(p);
        for s in SPECIES {
            self.gas[s as usize][i] += mix.mol[s as usize].max(0.0);
        }
        self.wake(i);
    }

    /// Remove a fraction (0..1) of the gas at a tile (wakes it). Returns the
    /// removed mixture — callers that want conservation ledger it.
    pub fn remove_fraction(&mut self, p: TilePos, frac: f32) -> GasMixture {
        let mut removed = GasMixture::default();
        if !self.in_bounds(p) {
            return removed;
        }
        let i = self.idx(p);
        let f = frac.clamp(0.0, 1.0);
        for s in SPECIES {
            let r = self.gas[s as usize][i] * f;
            self.gas[s as usize][i] -= r;
            removed.mol[s as usize] = r;
        }
        self.wake(i);
        removed
    }

    /// Overwrite the gas at a tile (wakes it). Used by dev tools to restore a
    /// standard fill; the caller is responsible for any ledger of the delta.
    pub fn set_mixture(&mut self, p: TilePos, mix: GasMixture) {
        if !self.in_bounds(p) {
            return;
        }
        let i = self.idx(p);
        for s in SPECIES {
            self.gas[s as usize][i] = mix.mol[s as usize].max(0.0);
        }
        self.wake(i);
    }

    /// Structural edit (wall built/torn, door built/torn, machine placed).
    /// Conservation rules:
    /// * becoming a wall: the tile's gas redistributes evenly into adjacent
    ///   cells reachable through an open boundary; anything that cannot be
    ///   placed (fully sealed pocket) stays stored in the tile's arrays,
    ///   inactive until the wall comes back down;
    /// * becoming floor/machine/door: the stored gas (usually the trapped
    ///   remainder, or zero for a torn-down wall) becomes active again and
    ///   the neighbours are woken — the new volume fills by real bulk flow,
    ///   never by spawning standard air.
    pub fn tile_changed(&mut self, map: &ShipMap, p: TilePos, new_tile: Tile) {
        if !self.in_bounds(p) {
            return;
        }
        let i = self.idx(p);
        if !is_gas_tile(Some(new_tile)) {
            // Floor/machine/door → wall: push the gas out to the adjacent
            // gas cells. The tile itself is already a wall, so the normal
            // boundary query would block every side — use the *old*
            // adjacency (neighbour is a gas cell) instead.
            let mix = self.mixture_at(p);
            let mut targets: Vec<usize> = Vec::new();
            for nb in [
                TilePos::new(p.x, p.y - 1),
                TilePos::new(p.x, p.y + 1),
                TilePos::new(p.x - 1, p.y),
                TilePos::new(p.x + 1, p.y),
            ] {
                if self.in_bounds(nb) && is_gas_tile(map.tile(nb)) {
                    targets.push(self.idx(nb));
                }
            }
            if !targets.is_empty() {
                let share = 1.0 / targets.len() as f32;
                for s in SPECIES {
                    self.gas[s as usize][i] = 0.0;
                }
                for &j in &targets {
                    for s in SPECIES {
                        self.gas[s as usize][j] += mix.mol[s as usize] * share;
                    }
                    self.wake(j);
                }
            }
            // else: fully enclosed — the gas stays stored, dormant, and
            // returns when the tile opens again. No ghost volume, no loss.
        } else {
            // Opening a new gas volume: wake it and the neighbours so real
            // flow fills it. Whatever is stored stays as the initial state.
            self.wake_around(p);
        }
        self.refresh_geometry(map);
    }

    // ---- transport -------------------------------------------------------------------

    /// One atmosphere sim step: vent, then bulk flow + advection + diffusion
    /// over awake cells. Deterministic for a fixed `dt` (speed-equivalent).
    #[allow(clippy::too_many_arguments)]
    pub fn step(
        &mut self,
        map: &ShipMap,
        thermal: &mut crate::thermal::ThermalGrid,
        devices: &crate::thermal::DeviceTiles,
        stats: &mut AtmoStats,
        dt: f32,
    ) {
        if dt <= 0.0 {
            return;
        }
        if self.geometry_version != map.version {
            self.refresh_geometry(map);
        }
        self.vent_pass(thermal, stats, dt);
        let current = self.take_awake();
        stats.active_cells = current.len();
        for &i in &current {
            let p = self.pos(i);
            // Fixed neighbour order keeps the pass deterministic.
            for nb in [
                TilePos::new(p.x, p.y - 1),
                TilePos::new(p.x, p.y + 1),
                TilePos::new(p.x - 1, p.y),
                TilePos::new(p.x + 1, p.y),
            ] {
                if !map.in_bounds(nb) {
                    continue;
                }
                if crate::airtight::boundary(map, p, nb) != crate::airtight::Boundary::Open {
                    continue;
                }
                let j = self.idx(nb);
                // Each unordered pair exactly once per step (same rule as the
                // thermal conduction pass).
                if self.is_awake(j) && j < i {
                    continue;
                }
                let moved = self.bulk_pair(i, j, thermal, devices, dt);
                let mixed = self.diffuse_pair(i, j, dt);
                if moved.max(mixed) > WAKE_EPS_MOL {
                    self.wake(j);
                }
            }
        }
        self.finish_step(&current);
        stats.steps += 1;
    }

    /// Exterior cells leak to vacuum: a fixed fraction of their gas leaves
    /// the ship per second, carrying its sensible heat. Exponential decay —
    /// no snapping needed, and the ledger books exactly what left.
    fn vent_pass(
        &mut self,
        thermal: &mut crate::thermal::ThermalGrid,
        stats: &mut AtmoStats,
        dt: f32,
    ) {
        if self.exposed.is_empty() {
            return;
        }
        let frac = (K_VENT * dt).min(1.0);
        for &i in &self.exposed.clone() {
            let t = self.total_at(i);
            if t <= EPS_MOL {
                continue;
            }
            let mut out = [0.0f32; 4];
            for (s, o) in out.iter_mut().enumerate() {
                let r = self.gas[s][i] * frac;
                self.gas[s][i] -= r;
                *o = r;
            }
            let mol_out: f32 = out.iter().sum();
            // Sensible heat of the vented gas leaves the ship's ledger.
            let q = (mol_out * GAS_CAP_PER_MOL) as f64 * (thermal.amb[i] + KELVIN_OFFSET) as f64;
            for (v, o) in stats.vented_mol.iter_mut().zip(out.iter()) {
                *v += *o as f64;
            }
            stats.vented_energy += q;
            stats.thermal_vented_energy += q;
            self.sync_gas_cap(thermal, i);
            thermal.wake(i);
            self.wake(i);
        }
    }

    /// Pressure-driven bulk flow between two open cells, equilibrium-clamped:
    /// `x_eq` is the amount that equalizes the pair's pressures exactly, the
    /// flux is a fixed fraction of it (so never overshoots), species move
    /// proportionally to the source composition, and the moved gas carries
    /// its sensible heat. Returns the moles moved (wake signal).
    fn bulk_pair(
        &mut self,
        i: usize,
        j: usize,
        thermal: &mut crate::thermal::ThermalGrid,
        devices: &crate::thermal::DeviceTiles,
        dt: f32,
    ) -> f32 {
        let (na, nb) = (self.total_at(i), self.total_at(j));
        if na + nb <= 2.0 * EPS_MOL {
            return 0.0;
        }
        // Amount that equalizes pressures: (na - x)·ta = (nb + x)·tb.
        let x_eq = eq_amount(
            na,
            thermal.amb[i],
            STANDARD_MOL,
            nb,
            thermal.amb[j],
            STANDARD_MOL,
        );
        if x_eq.abs() < EPS_MOL {
            return 0.0;
        }
        let delta = x_eq * (K_BULK * dt).min(1.0);
        if delta.abs() < EPS_MOL {
            return 0.0;
        }
        let (src, dst, amount) = if x_eq > 0.0 {
            (i, j, delta)
        } else {
            (j, i, -delta)
        };
        // One shared transfer primitive (species + sensible heat; the source
        // keeps its temperature, the destination mixes energies over its new
        // total capacity — gas + device mass share the node per Slice 3).
        let mut src_mix = [0.0f32; 4];
        let mut dst_mix = [0.0f32; 4];
        for s in 0..4 {
            src_mix[s] = self.gas[s][src];
            dst_mix[s] = self.gas[s][dst];
        }
        let mut dst_temp = thermal.amb[dst];
        move_gas(
            &mut src_mix,
            &mut dst_mix,
            thermal.amb[src],
            &mut dst_temp,
            devices.mass_at(dst),
            amount,
        );
        for s in 0..4 {
            self.gas[s][src] = src_mix[s];
            self.gas[s][dst] = dst_mix[s];
        }
        thermal.amb[dst] = dst_temp;
        self.sync_gas_cap(thermal, src);
        self.sync_gas_cap(thermal, dst);
        // Capacities and the destination temperature changed — thermal must
        // look at this pair again next step.
        thermal.wake(src);
        thermal.wake(dst);
        amount.abs()
    }

    /// Composition diffusion: equalize mole fractions per species, a fixed
    /// fraction of the equalizing amount per step. Conservative per species,
    /// symmetric, no thermal effect (documented simplification: mixing at
    /// equal pressure moves negligible net energy). Returns the largest
    /// species flux (wake signal).
    fn diffuse_pair(&mut self, i: usize, j: usize, dt: f32) -> f32 {
        let (na, nb) = (self.total_at(i), self.total_at(j));
        if na + nb <= 2.0 * EPS_MOL {
            return 0.0;
        }
        let lambda = (K_DIFF * dt).min(1.0);
        let pair_mass = (na * nb) / (na + nb);
        let mut biggest = 0.0f32;
        for s in 0..4 {
            let (fa, fb) = (self.gas[s][i] / na, self.gas[s][j] / nb);
            let d = fa - fb;
            if d.abs() < 1e-7 {
                continue;
            }
            let mv = lambda * d * pair_mass;
            self.gas[s][i] -= mv;
            self.gas[s][j] += mv;
            biggest = biggest.max(mv.abs());
        }
        biggest
    }

    /// Push one cell's gas heat capacity into the thermal grid (the thermal
    /// side's only atmosphere dependency).
    fn sync_gas_cap(&self, thermal: &mut crate::thermal::ThermalGrid, i: usize) {
        thermal.gas_cap[i] = (self.total_at(i) * GAS_CAP_PER_MOL).max(0.0);
    }

    /// Refresh every cell's thermal gas capacity (boot + tests).
    pub fn sync_all_gas_caps(&self, thermal: &mut crate::thermal::ThermalGrid) {
        for i in 0..thermal.gas_cap.len() {
            self.sync_gas_cap(thermal, i);
        }
    }

    // ---- accounting ------------------------------------------------------------------

    /// Total gas of one species still onboard (mol units).
    pub fn onboard(&self, s: Species) -> f64 {
        self.gas[s as usize].iter().map(|&v| v as f64).sum()
    }

    /// Total gas of one species onboard + vented (closed-system check).
    pub fn onboard_plus_vented(&self, stats: &AtmoStats, s: Species) -> f64 {
        self.onboard(s) + stats.vented_mol[s as usize]
    }
}

// =====================================================================================
// Ledger + cached UI summary
// =====================================================================================

/// Cumulative atmosphere accounting + per-step telemetry.
#[derive(Resource, Default, Debug, Clone)]
pub struct AtmoStats {
    /// Species totals at boot (retained-% reference; f64 accumulators).
    pub boot_mol: [f64; 4],
    /// Cumulative gas vented to space per species.
    pub vented_mol: [f64; 4],
    /// Cumulative sensible heat carried out by vented gas (H).
    pub vented_energy: f64,
    /// Same value mirrored for the thermal ledger invariant.
    pub thermal_vented_energy: f64,
    /// Awake cells in the last step (perf telemetry).
    pub active_cells: usize,
    /// Atmosphere steps executed (cadence bookkeeping).
    pub steps: u64,
}

impl AtmoStats {
    pub fn from_grid(grid: &AtmosphereGrid) -> Self {
        let mut me = Self::default();
        for s in SPECIES {
            me.boot_mol[s as usize] = grid.onboard(s);
        }
        me
    }

    /// Share of the boot gas still *onboard* (0..1) — vented and debug-removed
    /// gas counts as lost. (The conservation audit is a separate identity:
    /// `onboard + vented == boot`.)
    pub fn retained(&self, grid: &AtmosphereGrid) -> f32 {
        let boot: f64 = self.boot_mol.iter().sum();
        if boot <= 0.0 {
            return 1.0;
        }
        let now: f64 = SPECIES.iter().map(|&s| grid.onboard(s)).sum::<f64>();
        (now / boot).clamp(0.0, 1.0) as f32
    }

    /// Book a debug/scenario removal into the vent ledger so the
    /// closed-system identity (boot = onboard + vented) keeps holding while
    /// testing pressure differences by hand.
    pub fn debug_removed(&mut self, mix: &GasMixture) {
        for s in 0..4 {
            self.vented_mol[s] += mix.mol[s] as f64;
        }
    }
}

/// Worst composition hazard found by the last summary pass.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hazard {
    None,
    LowO2,
    HighCo2,
    Polluted,
}

impl Hazard {
    pub fn label(self) -> &'static str {
        match self {
            Hazard::None => "",
            Hazard::LowO2 => "LOW O2",
            Hazard::HighCo2 => "HIGH CO2",
            Hazard::Polluted => "POLLUTED",
        }
    }
}

/// Cached full-grid summary, recomputed on a sim-step cadence (never per
/// render frame) — the UI's only view of the grid.
#[derive(Resource, Default, Debug, Clone)]
pub struct AtmoSummary {
    pub min_pressure: f32,
    pub max_pressure: f32,
    pub min_o2_partial: f32,
    pub max_o2_partial: f32,
    pub max_co2_partial: f32,
    pub max_pollutant_partial: f32,
    /// Cells below the low-pressure threshold (excluding vacuum cells).
    pub low_cells: u32,
    /// Cells reading as vacuum.
    pub vacuum_cells: u32,
    /// Cells with O₂ partial pressure below the safe band.
    pub low_o2_cells: u32,
    pub high_co2_cells: u32,
    pub polluted_cells: u32,
    pub gas_cells: u32,
    pub retained: f32,
    pub active_cells: usize,
}

impl AtmoSummary {
    /// True when something in the grid warrants the always-on alert line.
    pub fn alert(&self) -> Option<&'static str> {
        if self.vacuum_cells > 0 || self.low_cells > 0 {
            Some("ATMOSPHERE LOSS — hull breach")
        } else if self.low_o2_cells > 0 {
            Some("LOW O2 — atmosphere")
        } else if self.high_co2_cells > 0 {
            Some("HIGH CO2 — atmosphere")
        } else if self.polluted_cells > 0 {
            Some("POLLUTED — atmosphere")
        } else {
            None
        }
    }
}

/// Player-facing safety bands (kPa partial pressure).
pub const O2_SAFE_KPA: f32 = 16.0;
pub const CO2_HIGH_KPA: f32 = 3.0;
pub const POLLUTANT_HIGH_KPA: f32 = 0.5;
/// Cells under this total pressure count as "low pressure" (not vacuum).
pub const LOW_PRESSURE_KPA: f32 = 70.0;

/// Recompute the cached summary from the grid. Called on a cadence by the
/// atmosphere system (and directly by tests).
pub fn summarize(
    grid: &AtmosphereGrid,
    thermal: &crate::thermal::ThermalGrid,
    stats: &AtmoStats,
) -> AtmoSummary {
    let mut s = AtmoSummary {
        min_pressure: f32::INFINITY,
        max_pressure: f32::NEG_INFINITY,
        min_o2_partial: f32::INFINITY,
        max_o2_partial: f32::NEG_INFINITY,
        max_co2_partial: 0.0,
        max_pollutant_partial: 0.0,
        retained: stats.retained(grid),
        active_cells: grid.awake_count(),
        ..default()
    };
    for i in 0..grid.gas[0].len() {
        let t = grid.total_at(i);
        if t <= EPS_MOL {
            continue;
        }
        s.gas_cells += 1;
        let p = pressure(t, thermal.amb[i]);
        s.min_pressure = s.min_pressure.min(p);
        s.max_pressure = s.max_pressure.max(p);
        let o2 = partial_pressure(grid.gas[0][i], t, thermal.amb[i]);
        s.min_o2_partial = s.min_o2_partial.min(o2);
        s.max_o2_partial = s.max_o2_partial.max(o2);
        if p < VACUUM_KPA {
            s.vacuum_cells += 1;
        } else if p < LOW_PRESSURE_KPA {
            s.low_cells += 1;
        }
        if o2 < O2_SAFE_KPA && p >= VACUUM_KPA {
            s.low_o2_cells += 1;
        }
        let co2 = partial_pressure(grid.gas[2][i], t, thermal.amb[i]);
        s.max_co2_partial = s.max_co2_partial.max(co2);
        if co2 > CO2_HIGH_KPA && p >= VACUUM_KPA {
            s.high_co2_cells += 1;
        }
        let pol = partial_pressure(grid.gas[3][i], t, thermal.amb[i]);
        s.max_pollutant_partial = s.max_pollutant_partial.max(pol);
        if pol > POLLUTANT_HIGH_KPA && p >= VACUUM_KPA {
            s.polluted_cells += 1;
        }
    }
    if s.gas_cells == 0 {
        s.min_pressure = 0.0;
        s.max_pressure = 0.0;
        s.min_o2_partial = 0.0;
        s.max_o2_partial = 0.0;
    }
    s
}

// =====================================================================================
// Overlay color (render + UI legend shared)
// =====================================================================================

/// Overlay heat-map color for a total pressure (kPa).
pub fn pressure_color(p: f32) -> Color {
    // Anchor points: (kPa, RGB). Vacuum → dark; low → blue; normal band
    // 90–110 → green/cyan; high → yellow; dangerous → red.
    const STOPS: [(f32, [f32; 3]); 6] = [
        (0.0, [0.05, 0.05, 0.10]),
        (40.0, [0.20, 0.30, 0.90]),
        (95.0, [0.20, 0.75, 0.55]),
        (110.0, [0.30, 0.90, 0.40]),
        (150.0, [0.95, 0.80, 0.25]),
        (220.0, [0.92, 0.25, 0.18]),
    ];
    let (mut lo, mut hi) = (STOPS[0], STOPS[STOPS.len() - 1]);
    for w in STOPS.windows(2) {
        if p >= w[0].0 && p <= w[1].0 {
            lo = w[0];
            hi = w[1];
        }
    }
    let f = if hi.0 == lo.0 {
        0.0
    } else {
        ((p - lo.0) / (hi.0 - lo.0)).clamp(0.0, 1.0)
    };
    Color::srgb(
        lo.1[0] + (hi.1[0] - lo.1[0]) * f,
        lo.1[1] + (hi.1[1] - lo.1[1]) * f,
        lo.1[2] + (hi.1[2] - lo.1[2]) * f,
    )
}

/// Composition-hazard tint for the overlay: pollutant (magenta) over high
/// CO₂ (orange) over low O₂ (pale blue-grey). `None` when the cell is clean.
pub fn hazard_color(mix: &GasMixture, temp_c: f32) -> Option<Color> {
    let t = mix.total();
    if t <= EPS_MOL {
        return None;
    }
    let pol = partial_pressure(mix.mol[3], t, temp_c);
    if pol > POLLUTANT_HIGH_KPA {
        return Some(Color::srgb(0.85, 0.30, 0.95));
    }
    let co2 = partial_pressure(mix.mol[2], t, temp_c);
    if co2 > CO2_HIGH_KPA {
        return Some(Color::srgb(0.95, 0.55, 0.20));
    }
    let o2 = partial_pressure(mix.mol[0], t, temp_c);
    if o2 < O2_SAFE_KPA {
        return Some(Color::srgb(0.45, 0.55, 0.75));
    }
    None
}

/// Carve a hull breach for testing/playtest: a hull-wall tile becomes floor
/// (with the usual thermal + gas bookkeeping), exposing its neighbours to
/// the exterior vacuum. Debug/scenario only — there is no hull-damage
/// gameplay this slice.
pub fn carve_breach(
    map: &mut ShipMap,
    thermal: &mut crate::thermal::ThermalGrid,
    atmo: &mut AtmosphereGrid,
    pos: TilePos,
) -> bool {
    if map.tile(pos) != Some(Tile::Wall) {
        return false;
    }
    map.set_tile(pos, Tile::Floor);
    thermal.tile_changed(pos, Tile::Floor);
    atmo.tile_changed(map, pos, Tile::Floor);
    atmo.wake_around(pos);
    true
}

// =====================================================================================
// Systems
// =====================================================================================

pub struct AtmospherePlugin;

impl Plugin for AtmospherePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AtmoStats>();
        app.init_resource::<AtmoSummary>();
        app.add_systems(
            FixedUpdate,
            // After the door system (a seal flip this step is visible to gas
            // this step — the Slice 5 door/atmosphere sync contract), after
            // the thermal pass (fresh temperatures and device masses), and
            // before movement. Advection writes temperatures the next thermal
            // step consumes; both orders are one-step-lagged somewhere, which
            // is deterministic and documented.
            atmosphere_system
                .after(crate::airtight::door_system)
                .before(crate::Set::Move)
                .in_set(crate::Set::Jobs),
        );
    }
}

/// Advance the atmosphere one sim step and refresh the cached UI summary on
/// a cadence. Guarded by `dt > 0`, so Pause freezes gas, diffusion, vents and
/// timers exactly like every other gameplay system.
#[allow(clippy::type_complexity)]
pub fn atmosphere_system(
    clock: Res<crate::simtime::SimClock>,
    map: Res<ShipMap>,
    mut grid: ResMut<AtmosphereGrid>,
    mut thermal: ResMut<crate::thermal::ThermalGrid>,
    devices: Res<crate::thermal::DeviceTiles>,
    mut stats: ResMut<AtmoStats>,
    mut summary: ResMut<AtmoSummary>,
) {
    let dt = clock.dt() as f32;
    grid.step(&map, &mut thermal, &devices, &mut stats, dt);
    // Summary cadence: every 30 sim steps (2× per real second at 1×), plus
    // the first step so the UI never shows an empty card.
    if stats.steps % 30 == 1 || summary.gas_cells == 0 {
        *summary = summarize(&grid, &thermal, &stats);
    }
}
