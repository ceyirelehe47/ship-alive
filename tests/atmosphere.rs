//! Atmosphere & pressure integration tests (Slice 5).
//!
//! These drive `AtmosphereGrid::step` directly on pure grids (map, thermal
//! and device mass) — the same function the app's `atmosphere_system` runs
//! every sim step — plus the structural-edit and thermal-coupling hooks.
//! Full-app acceptance (door wiring, UI, regressions) lives in the
//! `SLICE5_SCENARIO` driver.

use ship_alive::atmosphere::{
    partial_pressure, pressure, AtmoStats, AtmosphereGrid, GasMixture, Species, GAS_CAP_PER_MOL,
    O2_SAFE_KPA, POLLUTANT_HIGH_KPA,
};
use ship_alive::map::{ShipMap, Tile, TilePos};
use ship_alive::thermal::{DeviceTiles, ThermalGrid, AMB_CAP};

fn map_of(rows: &[&str]) -> ShipMap {
    ShipMap::from_layout(rows).0
}

/// Fresh atmosphere + thermal pair with gas caps synced.
fn world_of(rows: &[&str]) -> (ShipMap, ThermalGrid, AtmosphereGrid, AtmoStats) {
    let map = map_of(rows);
    let mut thermal = ThermalGrid::new(&map);
    let atmo = AtmosphereGrid::new(&map);
    atmo.sync_all_gas_caps(&mut thermal);
    let stats = AtmoStats::from_grid(&atmo);
    (map, thermal, atmo, stats)
}

fn step_n(
    map: &ShipMap,
    thermal: &mut ThermalGrid,
    atmo: &mut AtmosphereGrid,
    stats: &mut AtmoStats,
    n: usize,
) {
    let dev = DeviceTiles::sized((map.width * map.height) as usize);
    for _ in 0..n {
        atmo.step(map, thermal, &dev, stats, 1.0);
    }
}

fn species_sum(atmo: &AtmosphereGrid) -> [f64; 4] {
    ship_alive::atmosphere::SPECIES.map(|s| atmo.onboard(s))
}

fn total_mol(atmo: &AtmosphereGrid) -> f64 {
    species_sum(atmo).iter().sum()
}

// =====================================================================================
// Initialization, derivation, partial pressure (A / J / K)
// =====================================================================================

#[test]
fn standard_boot_pressure_and_composition() {
    let (_map, thermal, atmo, _stats) = world_of(&["#####", "#...#", "#####"]);
    for p in [TilePos::new(1, 1), TilePos::new(2, 1), TilePos::new(3, 1)] {
        let m = atmo.mixture_at(p);
        assert!((pressure(m.total(), thermal.amb[atmo.idx(p)]) - 101.325).abs() < 0.1);
        let o2pp = partial_pressure(m.mol[0], m.total(), thermal.amb[atmo.idx(p)]);
        assert!((o2pp - 21.0).abs() < 0.6, "O2 partial {o2pp}");
        assert!(m.mol[3] == 0.0, "no pollutant at boot");
        assert!(m.fraction(Species::Co2) < 0.01, "low CO2 at boot");
        assert!(m.fraction(Species::Inert) > 0.7, "mostly inert");
    }
    // Walls carry no gas.
    assert_eq!(atmo.mixture_at(TilePos::new(0, 0)).total(), 0.0);
}

#[test]
fn pressure_derivation_matches_definition() {
    // P = P_ref · (n/100) · (T+273.15)/294.15 exactly by construction.
    assert!((pressure(100.0, 21.0) - 101.325).abs() < 1e-3);
    assert!((pressure(50.0, 21.0) - 101.325 / 2.0).abs() < 1e-2);
    assert!((pressure(100.0, 0.0) - 101.325 * 273.15 / 294.15).abs() < 1e-2);
    // Hotter → higher pressure at the same amount; more gas → higher too.
    assert!(pressure(100.0, 80.0) > pressure(100.0, 21.0));
    assert!(pressure(150.0, 21.0) > pressure(100.0, 21.0));
    assert!(pressure(0.0, 300.0) < 0.001, "vacuum has no pressure");
}

#[test]
fn partial_pressure_low_total_high_fraction_still_low() {
    // 72 mol at 21 °C (~73 kPa, breathable pressure) but only 20% O2: the
    // partial pressure is below the safe band — the O2-danger answer.
    let total = 72.0f32;
    let o2 = 14.4f32;
    let pp = partial_pressure(o2, total, 21.0);
    assert!(pp < O2_SAFE_KPA, "{pp} must read unsafe");
    assert!((pp - pressure(total, 21.0) * 0.2).abs() < 1e-3);
    // Total pressure is fine — only O2 is low.
    assert!(pressure(total, 21.0) > 70.0);
    // Vacuum edge: no division blowups.
    assert_eq!(partial_pressure(0.0, 0.0, 21.0), 0.0);
}

// =====================================================================================
// Transport: bulk flow, overshoot, diffusion, doors, pollutants
// =====================================================================================

#[test]
fn bulk_flow_moves_gas_and_species_together() {
    let (map, mut thermal, mut atmo, mut stats) = world_of(&["#####", "#...#", "#####"]);
    let (a, b) = (TilePos::new(1, 1), TilePos::new(3, 1));
    atmo.set_mixture(
        a,
        GasMixture {
            mol: [80.0, 20.0, 0.0, 0.0],
        },
    ); // 100 mol, O2-rich
    atmo.set_mixture(
        b,
        GasMixture {
            mol: [0.0, 20.0, 0.0, 0.0],
        },
    ); // 20 mol
    atmo.sync_all_gas_caps(&mut thermal);
    let before = species_sum(&atmo);
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 400);
    let after = species_sum(&atmo);
    for s in 0..4 {
        assert!(
            (after[s] - before[s]).abs() < 1e-3,
            "species {s} not conserved: {} -> {}",
            before[s],
            after[s]
        );
    }
    let pa = atmo.pressure_at(a, &thermal);
    let pb = atmo.pressure_at(b, &thermal);
    assert!((pa - pb).abs() < 2.0, "not equalized: {pa} vs {pb}");
    // The middle cell took a share too — everything moved toward the low side.
    assert!(atmo.total_at(atmo.idx(b)) > 20.0);
}

#[test]
fn bulk_flow_never_overshoots_for_huge_dt() {
    let (map, mut thermal, mut atmo, mut stats) = world_of(&["####", "#..#", "####"]);
    let (a, b) = (TilePos::new(1, 1), TilePos::new(2, 1));
    atmo.set_mixture(
        a,
        GasMixture {
            mol: [200.0, 0.0, 0.0, 0.0],
        },
    );
    atmo.set_mixture(
        b,
        GasMixture {
            mol: [0.0, 0.0, 0.0, 0.0],
        },
    );
    atmo.sync_all_gas_caps(&mut thermal);
    let dev = DeviceTiles::sized(12);
    // A single enormous step must not cross equilibrium.
    atmo.step(&map, &mut thermal, &dev, &mut stats, 1_000.0);
    let (pa, pb) = (atmo.pressure_at(a, &thermal), atmo.pressure_at(b, &thermal));
    assert!(pa >= pb, "overshot: {pa} < {pb}");
    assert!((total_mol(&atmo) - 200.0).abs() < 1e-3, "gas lost");
    // And a long run stays monotone-ish and stable.
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 2000);
    assert!((atmo.pressure_at(a, &thermal) - atmo.pressure_at(b, &thermal)).abs() < 1.0);
    assert!((total_mol(&atmo) - 200.0).abs() < 1e-2);
}

#[test]
fn diffusion_mixes_composition_at_equal_total_pressure() {
    let (map, mut thermal, mut atmo, mut stats) = world_of(&["#####", "#...#", "#####"]);
    let (a, b) = (TilePos::new(1, 1), TilePos::new(3, 1));
    atmo.set_mixture(
        a,
        GasMixture {
            mol: [100.0, 0.0, 0.0, 0.0],
        },
    );
    atmo.set_mixture(
        b,
        GasMixture {
            mol: [0.0, 0.0, 100.0, 0.0],
        },
    );
    atmo.sync_all_gas_caps(&mut thermal);
    let before = species_sum(&atmo);
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 4000);
    let after = species_sum(&atmo);
    for s in 0..4 {
        assert!((after[s] - before[s]).abs() < 1e-2, "species {s} drifted");
    }
    let fa = atmo.mixture_at(a).fraction(Species::O2);
    let fb = atmo.mixture_at(b).fraction(Species::O2);
    assert!(
        (fa - fb).abs() < 0.05,
        "compositions did not mix: {fa} vs {fb}"
    );
    assert!(fa > 0.2 && fa < 0.8, "mixed to something in between: {fa}");
}

#[test]
fn closed_door_blocks_gas_exchange() {
    let (map, mut thermal, mut atmo, mut stats) =
        world_of(&["#######", "#.....#", "###D###", "#.....#", "#######"]);
    let (a, b) = (TilePos::new(3, 1), TilePos::new(3, 3));
    // Door boots closed (sealed): halve the whole upper room.
    for x in 1..=5 {
        let removed = atmo.remove_fraction(TilePos::new(x, 1), 0.5);
        stats.debug_removed(&removed);
    }
    atmo.sync_all_gas_caps(&mut thermal);
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 600);
    let (pa, pb) = (atmo.pressure_at(a, &thermal), atmo.pressure_at(b, &thermal));
    assert!(pa < 60.0, "low side leaked up: {pa}");
    assert!(pb > 95.0, "high side leaked down: {pb}");
    // The trapped door cell keeps its own standard fill while sealed.
    let door_mix = atmo.mixture_at(TilePos::new(3, 2));
    assert!((door_mix.total() - 100.0).abs() < 1.0);
}

#[test]
fn open_door_equalizes_gradually_not_instantly() {
    let (map, mut thermal, mut atmo, mut stats) =
        world_of(&["#######", "#.....#", "###D###", "#.....#", "#######"]);
    let (a, b) = (TilePos::new(3, 1), TilePos::new(3, 3));
    for x in 1..=5 {
        let removed = atmo.remove_fraction(TilePos::new(x, 1), 0.5);
        stats.debug_removed(&removed);
    }
    // Flip the door fully open the way door_system would.
    let mut map = map;
    map.set_door_state(
        TilePos::new(3, 2),
        ship_alive::map::DoorTileState {
            open: 1.0,
            locked: false,
        },
    );
    atmo.wake_around(TilePos::new(3, 2));
    atmo.sync_all_gas_caps(&mut thermal);

    // One step: exchange just started (no instant room average).
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 1);
    let pa1 = atmo.pressure_at(a, &thermal);
    assert!(
        pa1 < 60.0,
        "one step equalized the rooms: {pa1} (must propagate from the door)"
    );

    // Long run: converged, conserved (boot = onboard + ledgered removal).
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 3000);
    let (pa, pb) = (atmo.pressure_at(a, &thermal), atmo.pressure_at(b, &thermal));
    assert!((pa - pb).abs() < 3.0, "not equalized: {pa} vs {pb}");
    let audit = total_mol(&atmo) + stats.vented_mol.iter().sum::<f64>();
    let boot: f64 = stats.boot_mol.iter().sum();
    assert!((audit - boot).abs() < 1e-2, "audit {audit} vs boot {boot}");
}

#[test]
fn pollutant_spreads_only_through_open_boundaries() {
    let (map, mut thermal, mut atmo, mut stats) =
        world_of(&["#######", "#.....#", "###D###", "#.....#", "#######"]);
    atmo.inject(
        TilePos::new(3, 1),
        &GasMixture {
            mol: [0.0, 0.0, 0.0, 15.0],
        },
    );
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 300);
    // Sealed door: the far room stays clean, the near room keeps everything.
    assert_eq!(atmo.mixture_at(TilePos::new(3, 3)).mol[3], 0.0);
    let kept: f32 = (1..=5)
        .map(|x| atmo.mixture_at(TilePos::new(x, 1)).mol[3])
        .sum();
    assert!(kept > 14.9, "pollutant vanished: {kept}");
    // Open the door: it spreads (and stays conserved).
    let mut map = map;
    map.set_door_state(
        TilePos::new(3, 2),
        ship_alive::map::DoorTileState {
            open: 1.0,
            locked: false,
        },
    );
    atmo.wake_around(TilePos::new(3, 2));
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 3000);
    let far: f32 = (1..=5)
        .map(|x| atmo.mixture_at(TilePos::new(x, 3)).mol[3])
        .sum();
    assert!(far > 2.0, "pollutant did not spread: {far}");
    let pol = atmo.onboard(Species::Pollutant);
    assert!((pol - 15.0).abs() < 1e-2, "pollutant not conserved: {pol}");
}

// =====================================================================================
// Vacuum / decompression (G / H / I / M)
// =====================================================================================

/// One corridor with a carve-able hull segment on the left.
const BREACH_MAP: [&str; 3] = ["######", "#....#", "######"];

#[test]
fn decompression_loses_breach_tile_first_and_propagates() {
    let (mut map, mut thermal, mut atmo, mut stats) = world_of(&BREACH_MAP);
    ship_alive::atmosphere::carve_breach(&mut map, &mut thermal, &mut atmo, TilePos::new(0, 1));
    let boot = total_mol(&atmo);
    let near = TilePos::new(1, 1);
    let far = TilePos::new(4, 1);
    // After a few steps the near tile must be losing pressure faster.
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 8);
    let (pn, pf) = (
        atmo.pressure_at(near, &thermal),
        atmo.pressure_at(far, &thermal),
    );
    assert!(pn < pf, "breach tile not losing first: {pn} vs {pf}");
    assert!(
        pf > 40.0,
        "pressure wave reached the far end instantly: {pf}"
    );
    assert!(total_mol(&atmo) < boot, "no gas left the ship");
    assert!(
        stats.vented_mol.iter().sum::<f64>() > 0.0,
        "vent ledger empty"
    );
    // Long run: the corridor empties.
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 3000);
    assert!(atmo.pressure_at(far, &thermal) < 5.0, "room did not vent");
}

#[test]
fn vent_conservation_ledger_per_species() {
    let (mut map, mut thermal, mut atmo, mut stats) = world_of(&BREACH_MAP);
    ship_alive::atmosphere::carve_breach(&mut map, &mut thermal, &mut atmo, TilePos::new(0, 1));
    let boot = species_sum(&atmo);
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 2000);
    let now = species_sum(&atmo);
    for s in 0..4 {
        let audit = now[s] + stats.vented_mol[s];
        assert!(
            (audit - boot[s]).abs() < 1e-2,
            "species {s}: boot {} vs audit {}",
            boot[s],
            audit
        );
    }
}

#[test]
fn emergency_isolation_saves_the_room_behind_a_locked_door() {
    // Two 2x3 rooms split by a vertical wall with one E-W door; the right
    // room's outer hull gets carved open.
    let rows = ["#######", "#..#..#", "#..D..#", "#..#..#", "#######"];
    let (mut map, mut thermal, mut atmo, mut stats) = world_of(&rows);
    let door = TilePos::new(3, 2);
    assert_eq!(map.tile(door), Some(Tile::Door));
    // Lock the door closed (the door system keeps it sealed).
    map.set_door_state(
        door,
        ship_alive::map::DoorTileState {
            open: 0.0,
            locked: true,
        },
    );
    assert!(ship_alive::atmosphere::carve_breach(
        &mut map,
        &mut thermal,
        &mut atmo,
        TilePos::new(6, 2)
    ));
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 1500);
    let room = atmo.pressure_at(TilePos::new(1, 1), &thermal);
    let corridor = atmo.pressure_at(TilePos::new(5, 1), &thermal);
    assert!(room > 90.0, "locked door failed to save the room: {room}");
    assert!(corridor < 10.0, "corridor did not vent: {corridor}");
    // Re-opening equalizes again (I).
    map.set_door_state(
        door,
        ship_alive::map::DoorTileState {
            open: 1.0,
            locked: false,
        },
    );
    atmo.wake_around(door);
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 3000);
    let room2 = atmo.pressure_at(TilePos::new(1, 1), &thermal);
    let corridor2 = atmo.pressure_at(TilePos::new(5, 1), &thermal);
    assert!(
        (room2 - corridor2).abs() < 5.0,
        "no re-equalization: {room2} vs {corridor2}"
    );
}

// =====================================================================================
// Thermal integration (L / M / N / §20-24)
// =====================================================================================

#[test]
fn gas_heat_capacity_follows_amount_and_vacuum_has_none() {
    let (_map, mut thermal, mut atmo, _stats) = world_of(&["#####", "#...#", "#####"]);
    let a = TilePos::new(1, 1);
    let b = TilePos::new(2, 1);
    atmo.set_mixture(
        a,
        GasMixture {
            mol: [50.0, 50.0, 0.0, 0.0],
        },
    ); // 100 mol
    atmo.set_mixture(b, GasMixture::default()); // vacuum
    atmo.sync_all_gas_caps(&mut thermal);
    let ia = atmo.idx(a);
    let ib = atmo.idx(b);
    assert!((thermal.gas_cap[ia] - 100.0 * GAS_CAP_PER_MOL).abs() < 1e-4);
    assert!(thermal.gas_cap[ib] < 1e-4, "vacuum keeps gas capacity");
    // Device mass still counts on top of the gas capacity.
    assert!((thermal.air_cap_at(ia, 10.0) - (AMB_CAP + 10.0)).abs() < 1e-3);
}

#[test]
fn advective_heat_travels_with_the_gas() {
    let (map, mut thermal, mut atmo, mut stats) = world_of(&["####", "#..#", "####"]);
    let (hot, cold) = (TilePos::new(1, 1), TilePos::new(2, 1));
    // Hot over-pressured cell flows into a cold low-pressure cell.
    thermal.amb[atmo.idx(hot)] = 80.0;
    thermal.amb[atmo.idx(cold)] = 10.0;
    atmo.set_mixture(
        hot,
        GasMixture {
            mol: [150.0, 0.0, 0.0, 0.0],
        },
    );
    atmo.set_mixture(
        cold,
        GasMixture {
            mol: [10.0, 0.0, 0.0, 0.0],
        },
    );
    atmo.sync_all_gas_caps(&mut thermal);
    let devices = DeviceTiles::sized(12);
    let heat0 = thermal.total_heat(&devices);
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 30);
    let heat1 = thermal.total_heat(&devices);
    // Energy is conserved by advection (nothing vents here).
    assert!(
        (heat1 - heat0).abs() < 1e-2,
        "heat lost: {} -> {}",
        heat0,
        heat1
    );
    // The cold cell warmed because hot gas moved into it; the source keeps
    // its temperature (removing gas does not cool it — by design).
    assert!(
        thermal.amb[atmo.idx(cold)] > 15.0,
        "destination did not warm"
    );
    assert!(thermal.amb[atmo.idx(hot)] <= 80.0 + 1e-3);
}

#[test]
fn vented_gas_carries_its_heat_out() {
    let (mut map, mut thermal, mut atmo, mut stats) = world_of(&BREACH_MAP);
    ship_alive::atmosphere::carve_breach(&mut map, &mut thermal, &mut atmo, TilePos::new(0, 1));
    let devices = DeviceTiles::sized(18);
    let heat0 = thermal.total_heat(&devices);
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 60);
    let heat1 = thermal.total_heat(&devices);
    let lost = heat0 - heat1;
    // Δstored ≈ − vented_gas_energy (no radiators / injections here).
    assert!(
        (lost - stats.vented_energy).abs() < 1.0,
        "ledger mismatch: lost {lost} vs vented {}",
        stats.vented_energy
    );
    assert!(stats.vented_energy > 100.0, "vent carried no heat");
}

#[test]
fn low_pressure_gas_heats_faster_than_pressurized() {
    // Same heat into a standard cell and a near-vacuum cell: the thin one
    // responds harder (gas-dependent capacity).
    let (_map, mut thermal, mut atmo, _stats) = world_of(&["#####", "#...#", "#####"]);
    let std = TilePos::new(1, 1);
    let thin = TilePos::new(2, 1);
    atmo.set_mixture(
        thin,
        GasMixture {
            mol: [2.0, 2.0, 0.0, 0.0],
        },
    ); // 4 mol
    atmo.sync_all_gas_caps(&mut thermal);
    let q = 48.0f32; // one second of reactor idle heat
    let (is, it) = (atmo.idx(std), atmo.idx(thin));
    let (t0s, t0t) = (thermal.amb[is], thermal.amb[it]);
    thermal.amb[is] += q / thermal.air_cap_at(is, 0.0);
    thermal.amb[it] += q / thermal.air_cap_at(it, 0.0);
    let (d_std, d_thin) = (thermal.amb[is] - t0s, thermal.amb[it] - t0t);
    assert!(
        d_thin > d_std * 5.0,
        "vacuum cell not more sensitive: {d_thin} vs {d_std}"
    );
}

// =====================================================================================
// Structural edits (§89-92)
// =====================================================================================

#[test]
fn building_a_wall_redistributes_its_gas() {
    let (mut map, mut thermal, mut atmo, _stats) = world_of(&["#####", "#...#", "#####"]);
    let p = TilePos::new(2, 1);
    let before = species_sum(&atmo);
    map.set_tile(p, Tile::BuiltWall);
    thermal.tile_changed(p, Tile::BuiltWall);
    atmo.tile_changed(&map, p, Tile::BuiltWall);
    let after = species_sum(&atmo);
    for s in 0..4 {
        assert!(
            (after[s] - before[s]).abs() < 1e-3,
            "wall build destroyed gas {s}: {} -> {}",
            before[s],
            after[s]
        );
    }
    assert_eq!(atmo.mixture_at(p).total(), 0.0);
    // Neighbours got the share.
    assert!(atmo.total_at(atmo.idx(TilePos::new(1, 1))) > 100.0);
}

#[test]
fn tearing_a_wall_creates_vacuum_filled_by_flow() {
    let (mut map, mut thermal, mut atmo, mut stats) = world_of(&["#####", "#.#.#", "#####"]);
    let p = TilePos::new(2, 1);
    map.set_tile(p, Tile::Floor);
    thermal.tile_changed(p, Tile::Floor);
    atmo.tile_changed(&map, p, Tile::Floor);
    // No standard air spawned: the new volume starts (near) empty…
    assert!(atmo.total_at(atmo.idx(p)) < 1.0);
    // …and fills conservatively from the neighbours.
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 500);
    let filled = atmo.total_at(atmo.idx(p));
    assert!(filled > 60.0, "new volume did not fill: {filled}");
    let boot: f64 = stats.boot_mol.iter().sum();
    let audit = total_mol(&atmo) + stats.vented_mol.iter().sum::<f64>();
    assert!(
        (audit - boot).abs() < 1e-2,
        "gas not conserved: {audit} vs {boot}"
    );
}

#[test]
fn door_tile_keeps_gas_through_build_and_teardown() {
    let (mut map, mut thermal, mut atmo, _stats) =
        world_of(&["#######", "#.....#", "###.###", "#.....#", "#######"]);
    let p = TilePos::new(3, 2);
    let before = species_sum(&atmo);
    // Floor → door (built): the cell keeps its gas and becomes a portal.
    map.set_tile(p, Tile::Door);
    thermal.tile_changed(p, Tile::Door);
    atmo.tile_changed(&map, p, Tile::Door);
    assert!((atmo.mixture_at(p).total() - 100.0).abs() < 1.0);
    // Door → floor (torn down): gas unchanged, no duplication.
    map.set_tile(p, Tile::Floor);
    thermal.tile_changed(p, Tile::Floor);
    atmo.tile_changed(&map, p, Tile::Floor);
    let after = species_sum(&atmo);
    for s in 0..4 {
        assert!((after[s] - before[s]).abs() < 1e-3);
    }
}

// =====================================================================================
// Time behavior (O / P / Q) + robustness
// =====================================================================================

#[test]
fn pause_freezes_the_atmosphere() {
    let (map, mut thermal, mut atmo, mut stats) = world_of(&["#####", "#...#", "#####"]);
    atmo.remove_fraction(TilePos::new(1, 1), 0.5);
    atmo.sync_all_gas_caps(&mut thermal);
    let dev = DeviceTiles::sized(15);
    for _ in 0..120 {
        atmo.step(&map, &mut thermal, &dev, &mut stats, 0.0);
    }
    // dt = 0: nothing moved, nothing woke.
    assert!(atmo.pressure_at(TilePos::new(1, 1), &thermal) < 60.0);
    assert_eq!(atmo.awake_count(), 1, "only the woken cell, no propagation");
}

#[test]
fn fixed_steps_are_speed_independent() {
    // 120 steps of dt=1 vs 4 batches of 30 steps of dt=1: identical state.
    let run = |batches: usize, per: usize| -> Vec<f32> {
        let (map, mut thermal, mut atmo, mut stats) = world_of(&["#####", "#...#", "#####"]);
        atmo.remove_fraction(TilePos::new(1, 1), 0.6);
        atmo.sync_all_gas_caps(&mut thermal);
        for _ in 0..batches {
            step_n(&map, &mut thermal, &mut atmo, &mut stats, per);
        }
        let mut out = Vec::new();
        for x in 0..map.width {
            for y in 0..map.height {
                let p = TilePos::new(x, y);
                out.push(atmo.pressure_at(p, &thermal));
                out.push(atmo.mixture_at(p).mol[0]);
            }
        }
        out
    };
    let a = run(1, 120);
    let b = run(4, 30);
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        assert!((x - y).abs() < 1e-4, "diverged at {i}: {x} vs {y}");
    }
}

#[test]
fn stable_grid_sleeps_and_door_wakes_locally() {
    let (map, mut thermal, mut atmo, mut stats) =
        world_of(&["#######", "#.....#", "###D###", "#.....#", "#######"]);
    // Uniform boot: asleep immediately.
    assert_eq!(atmo.awake_count(), 0);
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 50);
    assert_eq!(atmo.awake_count(), 0, "uniform sealed ship stayed awake");
    // A seal flip wakes the door cell + neighbours only.
    let mut map = map;
    map.set_door_state(
        TilePos::new(3, 2),
        ship_alive::map::DoorTileState {
            open: 1.0,
            locked: false,
        },
    );
    atmo.wake_around(TilePos::new(3, 2));
    assert!(
        atmo.awake_count() <= 5,
        "woke the whole ship: {}",
        atmo.awake_count()
    );
    // Uniform pressures → the wake decays back to sleep.
    step_n(
        &map,
        &mut thermal,
        &mut atmo,
        &mut stats,
        WAKE_STEPS as usize + 10,
    );
    assert_eq!(atmo.awake_count(), 0, "never slept again");
}

use ship_alive::atmosphere::WAKE_STEPS;

#[test]
fn long_run_stays_numerically_sane() {
    let (map, mut thermal, mut atmo, mut stats) =
        world_of(&["#######", "#.....#", "###D###", "#.....#", "#######"]);
    let mut map = map;
    map.set_door_state(
        TilePos::new(3, 2),
        ship_alive::map::DoorTileState {
            open: 1.0,
            locked: false,
        },
    );
    for k in 0..20u32 {
        // Periodic perturbations so the system keeps working.
        atmo.inject(
            TilePos::new(1 + (k % 5) as i32, 1),
            &GasMixture {
                mol: [0.0, 0.0, 2.0, 1.0],
            },
        );
        thermal.amb[atmo.idx(TilePos::new(2, 1))] += 3.0;
        atmo.wake_at(TilePos::new(2, 1));
        step_n(&map, &mut thermal, &mut atmo, &mut stats, 1000);
    }
    for s in 0..4 {
        assert!(
            atmo.gas[s].iter().all(|&v| v.is_finite() && v >= 0.0),
            "species {s} invalid"
        );
    }
    for i in 0..atmo.gas[0].len() {
        let t = atmo.total_at(i);
        if t > 0.0 {
            let fr: f32 = atmo.gas.iter().map(|g| g[i] / t).sum();
            assert!((fr - 1.0).abs() < 1e-3, "fractions sum to {fr}");
            let p = pressure(t, thermal.amb[i]);
            assert!(p.is_finite() && (0.0..1e6).contains(&p));
        }
    }
}

// =====================================================================================
// Summary + hazards
// =====================================================================================

#[test]
fn summary_reports_hazards_and_retention() {
    let (_map, mut thermal, mut atmo, mut stats) = world_of(&["#####", "#...#", "#####"]);
    atmo.set_mixture(
        TilePos::new(1, 1),
        GasMixture {
            mol: [0.0, 100.0, 0.0, 0.0],
        },
    ); // no O2
    atmo.set_mixture(
        TilePos::new(3, 1),
        GasMixture {
            mol: [0.0, 90.0, 0.0, 10.0],
        },
    ); // pollutant
    atmo.sync_all_gas_caps(&mut thermal);
    let removed = atmo.remove_fraction(TilePos::new(2, 1), 0.9);
    stats.debug_removed(&removed);
    let s = ship_alive::atmosphere::summarize(&atmo, &thermal, &stats);
    assert!(s.min_o2_partial < O2_SAFE_KPA);
    assert!(s.max_pollutant_partial > POLLUTANT_HIGH_KPA);
    assert!(s.retained < 1.0);
    assert!(s.low_o2_cells >= 1);
    assert!(s.polluted_cells >= 1);
    assert_eq!(s.gas_cells, 3);
}

// =====================================================================================
// Performance (§116-119): printed numbers feed REPORT_ATMOSPHERE.md
// =====================================================================================

#[test]
fn perf_128_stable_map_sleeps_to_zero_cost() {
    let mut rows: Vec<String> = vec!["#".repeat(128)];
    for _ in 0..126 {
        rows.push(format!("#{}#", ".".repeat(126)));
    }
    rows.push("#".repeat(128));
    let refs: Vec<&str> = rows.iter().map(|s| s.as_str()).collect();
    let (map, mut thermal, mut atmo, mut stats) = world_of(&refs);
    // All-uniform boot: nothing to do. Let any initial wake settle.
    step_n(&map, &mut thermal, &mut atmo, &mut stats, 10);
    assert_eq!(atmo.awake_count(), 0, "stable map did not sleep");
    let dev = DeviceTiles::sized(128 * 128);
    let t0 = std::time::Instant::now();
    for _ in 0..1000 {
        atmo.step(&map, &mut thermal, &dev, &mut stats, 1.0);
    }
    let us = t0.elapsed().as_secs_f64() * 1e6 / 1000.0;
    println!(
        "PERF atmo 128x128 stable: {us:.2} us/step, active={}",
        atmo.awake_count()
    );
    assert!(us < 200.0, "sleeping step too expensive: {us} us");
}

#[test]
fn perf_128_active_pressure_front() {
    let mut rows: Vec<String> = vec!["#".repeat(128)];
    for _ in 0..126 {
        rows.push(format!("#{}#", ".".repeat(126)));
    }
    rows.push("#".repeat(128));
    let refs: Vec<&str> = rows.iter().map(|s| s.as_str()).collect();
    let (map, mut thermal, mut atmo, mut stats) = world_of(&refs);
    // Pressure event at the middle of the corridor.
    atmo.remove_fraction(TilePos::new(1, 64), 0.9);
    atmo.sync_all_gas_caps(&mut thermal);
    let dev = DeviceTiles::sized(128 * 128);
    let mut max_active = 0usize;
    let t0 = std::time::Instant::now();
    let steps = 2000;
    for _ in 0..steps {
        atmo.step(&map, &mut thermal, &dev, &mut stats, 1.0);
        max_active = max_active.max(atmo.awake_count());
    }
    let us = t0.elapsed().as_secs_f64() * 1e6 / steps as f64;
    println!(
        "PERF atmo 128x128 active front: {us:.2} us/step, peak_active={max_active}, final_active={}",
        atmo.awake_count()
    );
    // 240 steps/s at 4x must fit the frame budget with huge margin.
    assert!(us < 2000.0, "active step too expensive: {us} us");
}

#[test]
fn perf_many_sealed_rooms_stay_asleep() {
    // 16x16 grid of 7x7 sealed rooms; one room gets a pressure event.
    let mut rows = Vec::new();
    rows.push("#".repeat(113));
    for block in 0..16 {
        for _ in 0..6 {
            let mut row = String::from("#");
            for _ in 0..16 {
                row.push_str(&format!("{}#", ".".repeat(6)));
            }
            rows.push(row);
        }
        if block < 15 {
            rows.push("#".repeat(113));
        }
    }
    rows.push("#".repeat(113));
    assert_eq!(rows.len(), 113);
    let refs: Vec<&str> = rows.iter().map(|s| s.as_str()).collect();
    let (map, mut thermal, mut atmo, mut stats) = world_of(&refs);
    let dev = DeviceTiles::sized(113 * 113);
    // Wake one interior room only.
    for dy in 0..6 {
        for dx in 0..6 {
            atmo.wake_at(TilePos::new(1 + dx, 1 + dy));
        }
    }
    for _ in 0..3000 {
        atmo.step(&map, &mut thermal, &dev, &mut stats, 1.0);
    }
    let active = atmo.awake_count();
    println!("PERF atmo 256 sealed rooms after event: active={active}");
    assert!(active < 60, "inactive rooms got scanned: {active} active");
}
