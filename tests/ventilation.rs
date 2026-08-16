//! Ventilation & gas handling integration tests (Slice 6).
//!
//! These drive the full `ventilation_system` on a headless `World` (dense
//! grids + ECS devices), plus the structural release rules. Full-app wiring
//! (starter network, UI, regressions) lives in the `SLICE6_SCENARIO` driver.

use bevy::prelude::*;
use ship_alive::atmosphere::{
    pressure_vol, AtmoStats, AtmosphereGrid, GasMixture, GAS_CAP_PER_MOL,
};
use ship_alive::map::{ShipMap, Tile, TilePos};
use ship_alive::power::PowerStatus;
use ship_alive::simtime::SimClock;
use ship_alive::thermal::{DeviceTiles, ThermalGrid};
use ship_alive::ventilation::{
    release_tank_gas, remove_duct_preserving_gas, Blower, Dir4, DuctGrid, DuctTopology, GasTank,
    Vent, VentMode, VentStats, VentSummary, BLOWER_FLOW, BLOWER_HEAD_KPA, DUCT_MOL, TANK_HIGH_KPA,
    WAKE_STEPS,
};

fn map_of(rows: &[&str]) -> ShipMap {
    ShipMap::from_layout(rows).0
}

/// Headless world with all ventilation resources + one scheduled step of the
/// ventilation system.
struct Harness {
    world: World,
    schedule: Schedule,
}

fn room_map() -> Vec<&'static str> {
    vec!["########", "#......#", "#......#", "########"]
}

impl Harness {
    fn new(rows: &[&str]) -> Self {
        let map = map_of(rows);
        let n = (map.width * map.height) as usize;
        let mut thermal = ThermalGrid::new(&map);
        let atmo = AtmosphereGrid::new(&map);
        atmo.sync_all_gas_caps(&mut thermal);
        let (w, h) = (map.width, map.height);
        let mut world = World::new();
        world.insert_resource(map);
        world.insert_resource(thermal);
        world.insert_resource(atmo);
        world.insert_resource(AtmoStats::default());
        world.insert_resource(ship_alive::atmosphere::AtmoSummary::default());
        world.insert_resource(ship_alive::atmosphere::AtmoSummary::default());
        world.insert_resource(DuctGrid::new(w, h));
        world.insert_resource(DuctTopology::default());
        world.insert_resource(VentStats::default());
        world.insert_resource(VentSummary::default());
        world.insert_resource(DeviceTiles::sized(n));
        world.insert_resource(SimClock::default());
        let mut schedule = Schedule::default();
        // Rooms must re-equalize too (the vent's room cell refills from its
        // neighbours), exactly like the app's FixedUpdate order.
        schedule.add_systems((
            ship_alive::atmosphere::atmosphere_system,
            ship_alive::ventilation::ventilation_system,
        ));
        Self { world, schedule }
    }

    /// Lay a horizontal duct run on row `y` from `x0` to `x1` (inclusive).
    fn duct_row(&mut self, y: i32, x0: i32, x1: i32) {
        let tiles: Vec<TilePos> = (x0..=x1).map(|x| TilePos::new(x, y)).collect();
        let mut ducts = self.world.resource_mut::<DuctGrid>();
        for t in tiles {
            ducts.set(t, true);
        }
    }

    fn vent(&mut self, pos: TilePos, mode: VentMode, open: bool) -> Entity {
        self.world
            .spawn((
                pos,
                Vent {
                    mode,
                    open,
                    last_rate: 0.0,
                },
            ))
            .id()
    }

    fn blower(&mut self, pos: TilePos, dir: Dir4, powered: bool) -> Entity {
        let status = if powered {
            PowerStatus::Powered
        } else {
            PowerStatus::Unconnected
        };
        self.world.spawn((pos, Blower::new(dir), status)).id()
    }

    fn tank(&mut self, pos: TilePos, mix: GasMixture, valve_open: bool) -> Entity {
        self.world
            .spawn((
                pos,
                GasTank {
                    mix,
                    temp: 21.0,
                    valve_open,
                },
            ))
            .id()
    }

    fn step(&mut self, n: usize) {
        for _ in 0..n {
            self.world.resource_mut::<SimClock>().advance_sim(1.0);
            self.schedule.run(&mut self.world);
        }
    }

    fn ducts(&self) -> &DuctGrid {
        self.world.resource::<DuctGrid>()
    }

    fn atmo(&self) -> &AtmosphereGrid {
        self.world.resource::<AtmosphereGrid>()
    }

    fn thermal(&self) -> &ThermalGrid {
        self.world.resource::<ThermalGrid>()
    }

    fn set_duct_cell(&mut self, p: TilePos, mix: GasMixture, temp: f32) {
        let i = self.ducts().idx(p);
        let mut d = self.world.resource_mut::<DuctGrid>();
        for s in 0..4 {
            d.gas[s][i] = mix.mol[s];
        }
        d.temp[i] = temp;
        d.wake(i);
    }

    fn total_gas(&mut self) -> [f64; 4] {
        let atmo = self.atmo();
        let ducts = self.ducts();
        let mut out = ship_alive::atmosphere::SPECIES.map(|s| atmo.onboard(s));
        for (s, v) in out.iter_mut().enumerate() {
            *v += ducts.stored(s);
        }
        let tanks: Vec<GasTank> = self
            .world
            .query::<&GasTank>()
            .iter(&self.world)
            .copied()
            .collect();
        for t in &tanks {
            for (v, m) in out.iter_mut().zip(t.mix.mol.iter()) {
                *v += *m as f64;
            }
        }
        out
    }

    fn total_heat(&mut self) -> f64 {
        let th = self.thermal();
        let mut total = 0.0;
        for i in 0..th.amb.len() {
            total += th.gas_cap[i] as f64 * (th.amb[i] + 273.15) as f64;
        }
        total += self.ducts().heat();
        let tanks: Vec<GasTank> = self
            .world
            .query::<&GasTank>()
            .iter(&self.world)
            .copied()
            .collect();
        for t in &tanks {
            total += t.heat();
        }
        total
    }

    /// Take the four gas grids out of the world for a structural call.
    fn with_grids(
        &mut self,
        f: impl FnOnce(&ShipMap, &mut DuctGrid, &mut AtmosphereGrid, &mut ThermalGrid, &mut VentStats),
    ) {
        let map = self.world.resource::<ShipMap>().clone();
        let mut ducts = self.world.remove_resource::<DuctGrid>().unwrap();
        let mut atmo = self.world.remove_resource::<AtmosphereGrid>().unwrap();
        let mut thermal = self.world.remove_resource::<ThermalGrid>().unwrap();
        let mut vstats = self.world.remove_resource::<VentStats>().unwrap();
        f(&map, &mut ducts, &mut atmo, &mut thermal, &mut vstats);
        self.world.insert_resource(ducts);
        self.world.insert_resource(atmo);
        self.world.insert_resource(thermal);
        self.world.insert_resource(vstats);
    }
}

// =====================================================================================
// Duct volume / gas state / local flow
// =====================================================================================

#[test]
fn ducts_boot_empty_with_finite_volume() {
    let mut h = Harness::new(&room_map());
    h.duct_row(2, 2, 5);
    for x in 2..=5 {
        let p = TilePos::new(x, 2);
        assert_eq!(h.ducts().mixture_at(p).total(), 0.0, "new ducts are vacuum");
    }
    // Volume semantics: DUCT_MOL units at 21 °C = standard pressure.
    h.set_duct_cell(
        TilePos::new(2, 2),
        GasMixture {
            mol: [DUCT_MOL, 0.0, 0.0, 0.0],
        },
        21.0,
    );
    assert!((h.ducts().pressure_at(TilePos::new(2, 2)) - 101.325).abs() < 0.1);
}

#[test]
fn duct_flow_is_local_not_network_instant() {
    let mut h = Harness::new(&room_map());
    h.duct_row(2, 1, 6);
    let head = TilePos::new(1, 2);
    h.set_duct_cell(
        head,
        GasMixture {
            mol: [40.0, 40.0, 0.0, 0.0],
        },
        21.0,
    );
    let gas0 = h.total_gas();
    h.step(1);
    // After one step the far end has only a crumb (the in-pass Gauss-Seidel
    // cascade leaks a little further each step, but nothing like a network
    // average): the head still holds most of the gas.
    let head_total = h.ducts().mixture_at(head).total();
    let far = h.ducts().mixture_at(TilePos::new(6, 2)).total();
    assert!(
        far < head_total * 0.05,
        "instant mixing: far={far} head={head_total}"
    );
    assert!(h.ducts().mixture_at(TilePos::new(2, 2)).total() > 0.0);
    h.step(4000);
    let gas1 = h.total_gas();
    for s in 0..4 {
        assert!(
            (gas1[s] - gas0[s]).abs() < 1e-2,
            "species {s} not conserved"
        );
    }
    let (a, b) = (
        h.ducts().pressure_at(head),
        h.ducts().pressure_at(TilePos::new(6, 2)),
    );
    assert!((a - b).abs() < 2.0, "not equalized: {a} vs {b}");
    for s in 0..4 {
        assert!(h.ducts().gas[s].iter().all(|&v| v >= 0.0));
    }
}

// =====================================================================================
// Vents
// =====================================================================================

#[test]
fn vent_modes_honor_direction_and_closed_transfers_nothing() {
    let mut h = Harness::new(&room_map());
    h.duct_row(2, 2, 5);
    let vent_pos = TilePos::new(3, 2);
    let ri = h.atmo().idx(vent_pos);
    h.set_duct_cell(
        vent_pos,
        GasMixture {
            mol: [DUCT_MOL, 0.0, 0.0, 0.0],
        },
        21.0,
    );
    h.world.resource_mut::<AtmosphereGrid>().set_mixture(
        vent_pos,
        GasMixture {
            mol: [10.0, 0.0, 0.0, 0.0],
        },
    );

    // Supply: duct → room only. Duct above room pressure → flows.
    let v = h.vent(vent_pos, VentMode::Supply, true);
    h.step(1);
    let rate = h.world.get::<Vent>(v).unwrap().last_rate;
    assert!(rate > 0.0, "supply must flow from high duct to low room");
    assert!(h.atmo().total_at(ri) > 10.0);

    // Closed: no vent transfer (the duct total is the vent-isolated
    // measure — room cells keep re-equalizing among themselves).
    h.world.get_mut::<Vent>(v).unwrap().open = false;
    h.step(50);
    let frozen: f32 = (1..=6)
        .map(|x| h.ducts().mixture_at(TilePos::new(x, 2)).total())
        .sum();
    h.step(50);
    let now: f32 = (1..=6)
        .map(|x| h.ducts().mixture_at(TilePos::new(x, 2)).total())
        .sum();
    assert!(
        (now - frozen).abs() < 1e-3,
        "closed vent transferred gas: {frozen} -> {now}"
    );

    // Exhaust against a higher duct pressure: no transfer at all (the duct
    // total is the vent-isolated measure; room cells keep re-equalizing
    // among themselves through the atmosphere system).
    {
        let mut vent = h.world.get_mut::<Vent>(v).unwrap();
        vent.mode = VentMode::Exhaust;
        vent.open = true;
    }
    // Make the whole duct the high side (15 units per cell = ~150 kPa) so
    // duct-internal spreading cannot drag the vent cell below the room.
    for x in 2..=5 {
        h.set_duct_cell(
            TilePos::new(x, 2),
            GasMixture {
                mol: [15.0, 0.0, 0.0, 0.0],
            },
            21.0,
        );
    }
    h.step(2); // let duct-internal settling finish
    let duct_before: f32 = (2..=5)
        .map(|x| h.ducts().mixture_at(TilePos::new(x, 2)).total())
        .sum();
    h.step(30);
    let duct_after: f32 = (2..=5)
        .map(|x| h.ducts().mixture_at(TilePos::new(x, 2)).total())
        .sum();
    assert!(
        (duct_after - duct_before).abs() < 1e-2,
        "exhaust supplied the room against the gradient: {duct_before} -> {duct_after}"
    );
}

// =====================================================================================
// Blowers
// =====================================================================================

#[test]
fn blower_power_direction_and_cap() {
    let mut h = Harness::new(&room_map());
    h.duct_row(2, 1, 6);
    let bpos = TilePos::new(3, 2);
    h.set_duct_cell(
        TilePos::new(2, 2),
        GasMixture {
            mol: [30.0, 30.0, 0.0, 0.0],
        },
        21.0,
    );

    // Unpowered: no push.
    let b = h.blower(bpos, Dir4::East, false);
    h.step(3);
    assert_eq!(
        h.world.get::<Blower>(b).unwrap().last_flow,
        0.0,
        "unpowered blower must not pump"
    );

    // Powered east: pushes, capped per step.
    h.world.get_mut::<Blower>(b).unwrap().dir = Dir4::East;
    h.world.entity_mut(b).insert(PowerStatus::Powered);
    h.set_duct_cell(
        TilePos::new(2, 2),
        GasMixture {
            mol: [30.0, 30.0, 0.0, 0.0],
        },
        21.0,
    );
    h.step(1);
    let flow = h.world.get::<Blower>(b).unwrap().last_flow;
    assert!(flow > 0.0, "powered blower must push");
    assert!(flow <= BLOWER_FLOW + 1e-3, "blower exceeded its cap");

    // Reversed: a westbound blower feeds its west neighbour from its own
    // cell, leaving the east side untouched.
    h.world.get_mut::<Blower>(b).unwrap().dir = Dir4::West;
    for x in 2..=6 {
        h.set_duct_cell(
            TilePos::new(x, 2),
            if x == 3 {
                GasMixture {
                    mol: [20.0, 0.0, 0.0, 0.0],
                }
            } else {
                GasMixture::default()
            },
            21.0,
        );
    }
    h.step(1);
    assert!(
        h.world.get::<Blower>(b).unwrap().last_flow > 0.0,
        "west blower did not run"
    );
    let (west, east) = (
        h.ducts().mixture_at(TilePos::new(2, 2)).total(),
        h.ducts().mixture_at(TilePos::new(4, 2)).total(),
    );
    // Passive equalization feeds both sides; the blower must bias the west.
    assert!(
        west > east,
        "west blower did not bias its push west: west={west} east={east}"
    );

    // Dead-end stall: pushing into a blocked side stops at the head.
    let mut h2 = Harness::new(&room_map());
    h2.duct_row(2, 2, 3);
    h2.blower(TilePos::new(2, 2), Dir4::East, true);
    h2.set_duct_cell(
        TilePos::new(2, 2),
        GasMixture {
            mol: [10.0, 0.0, 0.0, 0.0],
        },
        21.0,
    );
    h2.step(2000);
    let (p_in, p_out) = (
        h2.ducts().pressure_at(TilePos::new(2, 2)),
        h2.ducts().pressure_at(TilePos::new(3, 2)),
    );
    assert!(
        p_out - p_in <= BLOWER_HEAD_KPA + 1.0,
        "dead-end exceeded the head: {p_in} -> {p_out}"
    );
}

// =====================================================================================
// Tanks
// =====================================================================================

#[test]
fn tank_volume_pressure_valve_and_mixture() {
    let mut h = Harness::new(&room_map());
    h.duct_row(2, 2, 5);
    let tpos = TilePos::new(3, 2);
    let mix = GasMixture {
        mol: [80.0, 240.0, 60.0, 20.0],
    }; // 400 units, mixed
    h.tank(tpos, mix, true);
    {
        let t = h.world.query::<&GasTank>().single(&h.world).unwrap();
        assert!((t.pressure() - 101.325).abs() < 0.2, "400 units = standard");
    }

    // Empty ducts: the tank charges them through the valve.
    h.step(4000);
    let duct_total: f32 = (2..=5)
        .map(|x| h.ducts().mixture_at(TilePos::new(x, 2)).total())
        .sum();
    let gas0 = h.total_gas();
    assert!(duct_total > 25.0, "ducts did not fill from the tank");
    assert!(
        h.world
            .query::<&GasTank>()
            .single(&h.world)
            .unwrap()
            .total()
            < 400.0,
        "tank did not discharge"
    );
    // Species conserved overall and the mixture ratios survive the trip.
    let gas1 = h.total_gas();
    for s in 0..4 {
        assert!((gas1[s] - gas0[s]).abs() < 1e-2);
    }
    let d = h.ducts().mixture_at(TilePos::new(4, 2));
    let fr = d.total();
    assert!((d.mol[0] / fr - 0.2).abs() < 0.02, "O2 share preserved");
    assert!((d.mol[2] / fr - 0.15).abs() < 0.02, "CO2 share preserved");
    assert!(
        (d.mol[3] / fr - 0.05).abs() < 0.02,
        "pollutant share preserved"
    );

    // Valve closed: the tank is isolated from further duct changes.
    h.world
        .query::<&mut GasTank>()
        .single_mut(&mut h.world)
        .unwrap()
        .valve_open = false;
    let frozen = h
        .world
        .query::<&GasTank>()
        .single(&h.world)
        .unwrap()
        .total();
    h.set_duct_cell(TilePos::new(4, 2), GasMixture::default(), 21.0);
    h.step(200);
    let now = h
        .world
        .query::<&GasTank>()
        .single(&h.world)
        .unwrap()
        .total();
    assert!(
        (now - frozen).abs() < 0.5,
        "closed valve leaked {frozen} -> {now}"
    );
}

// =====================================================================================
// Conservation across the full chain + heat
// =====================================================================================

#[test]
fn room_to_duct_to_room_round_trip_conserves_species_and_heat() {
    let mut h = Harness::new(&room_map());
    h.duct_row(2, 2, 5);
    let a = TilePos::new(2, 2);
    let b = TilePos::new(5, 2);
    h.vent(a, VentMode::Exhaust, true);
    h.vent(b, VentMode::Supply, true);
    h.blower(TilePos::new(3, 2), Dir4::East, true);
    let ai = h.atmo().idx(a);
    h.world.resource_mut::<ThermalGrid>().amb[ai] = 60.0;
    let gas0 = h.total_gas();
    let heat0 = h.total_heat();
    h.step(3000);
    let gas1 = h.total_gas();
    let heat1 = h.total_heat();
    for s in 0..4 {
        assert!(
            (gas1[s] - gas0[s]).abs() < 5e-2,
            "species {s}: {} -> {}",
            gas0[s],
            gas1[s]
        );
    }
    assert!(
        (heat1 - heat0).abs() < 1.0,
        "heat not conserved: {heat0} vs {heat1}"
    );
    let bi = h.atmo().idx(b);
    assert!(
        h.thermal().amb[bi] > 22.5,
        "destination did not warm: {}",
        h.thermal().amb[bi]
    );
}

#[test]
fn hot_gas_into_duct_carries_energy() {
    let mut h = Harness::new(&room_map());
    h.duct_row(2, 2, 4);
    let vp = TilePos::new(3, 2);
    let ri = h.atmo().idx(vp);
    h.world.resource_mut::<ThermalGrid>().amb[ri] = 90.0;
    h.vent(vp, VentMode::Exhaust, true);
    let heat0 = h.total_heat();
    h.step(200);
    let heat1 = h.total_heat();
    assert!((heat1 - heat0).abs() < 0.5, "energy created/destroyed");
    let di = h.ducts().idx(vp);
    assert!(
        h.ducts().temp[di] > 40.0,
        "duct gas did not warm: {}",
        h.ducts().temp[di]
    );
}

// =====================================================================================
// Topology
// =====================================================================================

#[test]
fn topology_split_merge_and_no_rebuild_on_flow() {
    let mut h = Harness::new(&room_map());
    h.duct_row(2, 1, 6);
    h.set_duct_cell(
        TilePos::new(1, 2),
        GasMixture {
            mol: [10.0, 0.0, 0.0, 0.0],
        },
        21.0,
    );
    h.step(10);
    assert_eq!(h.world.resource::<DuctTopology>().nets, 1);
    let rebuilds = h.world.resource::<DuctTopology>().rebuilds;
    h.step(200);
    assert_eq!(h.world.resource::<DuctTopology>().rebuilds, rebuilds);
    // Cut in the middle: two networks.
    let cut = TilePos::new(4, 2);
    h.with_grids(|map, ducts, atmo, thermal, vstats| {
        remove_duct_preserving_gas(map, ducts, atmo, thermal, vstats, cut);
    });
    h.step(5);
    assert_eq!(h.world.resource::<DuctTopology>().nets, 2);
    // Reconnect: one network again.
    h.world.resource_mut::<DuctGrid>().set(cut, true);
    h.step(5);
    assert_eq!(h.world.resource::<DuctTopology>().nets, 1);
}

#[test]
fn independent_networks_never_cross_transfer() {
    let mut h = Harness::new(&[
        "############",
        "#..........#",
        "#..........#",
        "############",
    ]);
    h.duct_row(2, 2, 4);
    h.duct_row(2, 7, 9);
    h.set_duct_cell(
        TilePos::new(2, 2),
        GasMixture {
            mol: [10.0, 0.0, 0.0, 0.0],
        },
        21.0,
    );
    h.step(2000);
    assert_eq!(h.world.resource::<DuctTopology>().nets, 2);
    for x in 7..=9 {
        assert_eq!(
            h.ducts().mixture_at(TilePos::new(x, 2)).total(),
            0.0,
            "gas jumped networks"
        );
    }
}

// =====================================================================================
// Structural release rules
// =====================================================================================

#[test]
fn duct_removal_preserves_gas_into_neighbours_room_or_ledger() {
    // Neighbour case.
    let mut h = Harness::new(&room_map());
    h.duct_row(2, 2, 4);
    let p = TilePos::new(3, 2);
    h.set_duct_cell(
        p,
        GasMixture {
            mol: [4.0, 3.0, 2.0, 1.0],
        },
        40.0,
    );
    let before = h.total_gas();
    h.with_grids(|map, ducts, atmo, thermal, vstats| {
        remove_duct_preserving_gas(map, ducts, atmo, thermal, vstats, p);
    });
    let after = h.total_gas();
    for s in 0..4 {
        assert!(
            (after[s] - before[s]).abs() < 1e-3,
            "neighbour release lost species {s}"
        );
    }
    assert!(!h.ducts().has(p));

    // Room release case: a lone charged duct tile under floor.
    let mut h2 = Harness::new(&room_map());
    h2.duct_row(2, 3, 3);
    h2.set_duct_cell(
        TilePos::new(3, 2),
        GasMixture {
            mol: [3.0, 2.0, 1.0, 0.5],
        },
        30.0,
    );
    let (gas0, heat0) = (h2.total_gas(), h2.total_heat());
    h2.with_grids(|map, ducts, atmo, thermal, vstats| {
        remove_duct_preserving_gas(map, ducts, atmo, thermal, vstats, TilePos::new(3, 2));
    });
    let (gas1, heat1) = (h2.total_gas(), h2.total_heat());
    for s in 0..4 {
        assert!(
            (gas1[s] - gas0[s]).abs() < 1e-3,
            "room release lost species {s}"
        );
    }
    assert!((heat1 - heat0).abs() < 0.5, "room release lost energy");
}

#[test]
fn tank_release_conserves_into_duct() {
    let map = map_of(&room_map());
    let mut thermal = ThermalGrid::new(&map);
    let mut atmo = AtmosphereGrid::new(&map);
    atmo.sync_all_gas_caps(&mut thermal);
    let mut ducts = DuctGrid::new(map.width, map.height);
    ducts.set(TilePos::new(3, 2), true);
    let mut vstats = VentStats::default();
    let tank = GasTank {
        mix: GasMixture {
            mol: [40.0, 30.0, 20.0, 10.0],
        },
        temp: 50.0,
        valve_open: true,
    };
    release_tank_gas(
        &map,
        Some(&mut ducts),
        Some(&mut atmo),
        &mut thermal,
        Some(&mut vstats),
        tank,
        TilePos::new(3, 2),
    );
    // Everything landed in the duct under the tank.
    let got = ducts.mixture_at(TilePos::new(3, 2));
    assert!((got.mol[0] - 40.0).abs() < 1e-3);
    assert!((got.mol[3] - 10.0).abs() < 1e-3);
    assert_eq!(vstats.vented_mol.iter().sum::<f64>(), 0.0);
    // The tank warning threshold (250 kPa) sits well above room pressure.
    assert!((TANK_HIGH_KPA - 250.0).abs() < f32::EPSILON);
}

// =====================================================================================
// Breach coupling + isolation
// =====================================================================================

#[test]
fn breach_through_open_vent_drains_the_tank() {
    // A room whose left wall is carved open to space.
    let mut map = map_of(&["######", "#..###", "#..###", "######"]);
    let mut thermal = ThermalGrid::new(&map);
    let mut atmo = AtmosphereGrid::new(&map);
    atmo.sync_all_gas_caps(&mut thermal);
    ship_alive::atmosphere::carve_breach(&mut map, &mut thermal, &mut atmo, TilePos::new(0, 2));
    let n = (map.width * map.height) as usize;
    let mut h = Harness {
        world: World::new(),
        schedule: Schedule::default(),
    };
    let (w, hh) = (map.width, map.height);
    h.world.insert_resource(map);
    h.world.insert_resource(thermal);
    h.world.insert_resource(atmo);
    h.world.insert_resource(AtmoStats::default());
    h.world
        .insert_resource(ship_alive::atmosphere::AtmoSummary::default());
    h.world.insert_resource(DuctGrid::new(w, hh));
    h.world.insert_resource(DuctTopology::default());
    h.world.insert_resource(VentStats::default());
    h.world.insert_resource(VentSummary::default());
    h.world.insert_resource(DeviceTiles::sized(n));
    h.world.insert_resource(SimClock::default());
    h.schedule.add_systems((
        ship_alive::atmosphere::atmosphere_system,
        ship_alive::ventilation::ventilation_system,
    ));
    h.duct_row(2, 1, 2);
    h.vent(TilePos::new(1, 2), VentMode::Balanced, true);
    h.tank(TilePos::new(2, 2), GasTank::prefilled_standard().mix, true);
    let t0 = h
        .world
        .query::<&GasTank>()
        .single(&h.world)
        .unwrap()
        .total();
    h.step(3000);
    let t1 = h
        .world
        .query::<&GasTank>()
        .single(&h.world)
        .unwrap()
        .total();
    let vented = h
        .world
        .resource::<AtmoStats>()
        .vented_mol
        .iter()
        .sum::<f64>();
    assert!(vented > 100.0, "nothing vented to space");
    assert!(
        t1 < t0 - 5.0,
        "tank not drained through the vent: {t0} -> {t1}"
    );

    // Isolation: close the vent + valve; the tank holds.
    h.world
        .query::<&mut Vent>()
        .single_mut(&mut h.world)
        .unwrap()
        .open = false;
    h.world
        .query::<&mut GasTank>()
        .single_mut(&mut h.world)
        .unwrap()
        .valve_open = false;
    let frozen = h
        .world
        .query::<&GasTank>()
        .single(&h.world)
        .unwrap()
        .total();
    h.step(3000);
    let now = h
        .world
        .query::<&GasTank>()
        .single(&h.world)
        .unwrap()
        .total();
    assert!(
        (now - frozen).abs() < 1.0,
        "isolated tank drained {frozen} -> {now}"
    );
}

// =====================================================================================
// Time behaviour
// =====================================================================================

#[test]
fn pause_freezes_ventilation() {
    let mut h = Harness::new(&room_map());
    h.duct_row(2, 2, 4);
    h.set_duct_cell(
        TilePos::new(2, 2),
        GasMixture {
            mol: [10.0, 0.0, 0.0, 0.0],
        },
        21.0,
    );
    h.world.resource_mut::<SimClock>().advance_sim(0.0);
    for _ in 0..100 {
        h.schedule.run(&mut h.world);
    }
    assert_eq!(
        h.ducts().mixture_at(TilePos::new(3, 2)).total(),
        0.0,
        "gas moved while paused"
    );
}

#[test]
fn fixed_steps_are_speed_independent() {
    let run = |batches: usize, per: usize| -> Vec<f32> {
        let mut h = Harness::new(&room_map());
        h.duct_row(2, 2, 5);
        h.tank(TilePos::new(4, 2), GasTank::prefilled_standard().mix, true);
        h.vent(TilePos::new(3, 2), VentMode::Balanced, true);
        for _ in 0..batches {
            h.step(per);
        }
        let mut out = Vec::new();
        for x in 1..=6 {
            out.push(h.ducts().mixture_at(TilePos::new(x, 2)).total());
        }
        out.push(
            h.world
                .query::<&GasTank>()
                .single(&h.world)
                .unwrap()
                .total(),
        );
        out
    };
    let a = run(1, 600);
    let b = run(4, 150);
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        assert!((x - y).abs() < 1e-3, "diverged at {i}: {x} vs {y}");
    }
}

#[test]
fn sealed_network_sleeps() {
    let mut h = Harness::new(&room_map());
    h.duct_row(2, 2, 5);
    h.set_duct_cell(
        TilePos::new(3, 2),
        GasMixture {
            mol: [10.0, 0.0, 0.0, 0.0],
        },
        21.0,
    );
    h.step(WAKE_STEPS as usize + 50);
    assert_eq!(h.ducts().awake_count(), 0, "sealed network never slept");
}

// =====================================================================================
// Performance
// =====================================================================================

fn open_room_map(size: usize) -> Vec<String> {
    let mut r: Vec<String> = vec!["#".repeat(size)];
    for _ in 1..size - 1 {
        r.push(format!("#{}#", ".".repeat(size - 2)));
    }
    r.push("#".repeat(size));
    r
}

fn big_world(size: usize) -> (World, Schedule) {
    let rows = open_room_map(size);
    let refs: Vec<&str> = rows.iter().map(|s| s.as_str()).collect();
    let map = map_of(&refs);
    let n = (map.width * map.height) as usize;
    let mut thermal = ThermalGrid::new(&map);
    let atmo = AtmosphereGrid::new(&map);
    atmo.sync_all_gas_caps(&mut thermal);
    let mut world = World::new();
    world.insert_resource(map);
    world.insert_resource(thermal);
    world.insert_resource(atmo);
    world.insert_resource(AtmoStats::default());
    world.insert_resource(ship_alive::atmosphere::AtmoSummary::default());
    let mut ducts = DuctGrid::new(size as i32, size as i32);
    for y in (2..size as i32 - 2).step_by(2) {
        for x in 1..size as i32 - 1 {
            ducts.set(TilePos::new(x, y), true);
        }
    }
    world.insert_resource(ducts);
    world.insert_resource(DuctTopology::default());
    world.insert_resource(VentStats::default());
    world.insert_resource(VentSummary::default());
    world.insert_resource(DeviceTiles::sized(n));
    world.insert_resource(SimClock::default());
    let mut schedule = Schedule::default();
    schedule.add_systems(ship_alive::ventilation::ventilation_system);
    (world, schedule)
}

fn drive(world: &mut World, schedule: &mut Schedule, steps: usize) {
    for _ in 0..steps {
        world.resource_mut::<SimClock>().advance_sim(1.0);
        schedule.run(world);
    }
}

#[test]
fn perf_128_stable_duct_grid_sleeps() {
    let (mut world, mut schedule) = big_world(128);
    // Uniform standard fill: real gas, no gradients.
    {
        let mut ducts = world.resource_mut::<DuctGrid>();
        for i in 0..ducts.gas[0].len() {
            if ducts.is_duct_index(i) {
                for (s, v) in ship_alive::atmosphere::STANDARD_MIX.iter().enumerate() {
                    ducts.gas[s][i] = v * DUCT_MOL / ship_alive::atmosphere::STANDARD_MOL;
                }
            }
        }
    }
    drive(&mut world, &mut schedule, WAKE_STEPS as usize + 20);
    assert_eq!(
        world.resource::<DuctGrid>().awake_count(),
        0,
        "uniform duct grid never slept"
    );
    let t0 = std::time::Instant::now();
    drive(&mut world, &mut schedule, 1000);
    let us = t0.elapsed().as_secs_f64() * 1e6 / 1000.0;
    println!("PERF vent 128x128 stable ducts: {us:.2} us/step");
    assert!(us < 300.0, "sleeping duct step too expensive: {us} us");
}

#[test]
fn perf_128_active_transport() {
    let (mut world, mut schedule) = big_world(128);
    // Over-pressure the left column of each duct row → a long transient.
    {
        let mut ducts = world.resource_mut::<DuctGrid>();
        for y in (2..126).step_by(2) {
            let i = ducts.idx(TilePos::new(1, y));
            for (s, v) in ship_alive::atmosphere::STANDARD_MIX.iter().enumerate() {
                ducts.gas[s][i] = v * DUCT_MOL * 3.0 / ship_alive::atmosphere::STANDARD_MOL;
            }
            ducts.wake(i);
        }
    }
    let mut max_active = 0usize;
    let mut max_edges = 0usize;
    let t0 = std::time::Instant::now();
    let steps = 2000;
    for _ in 0..steps {
        world.resource_mut::<SimClock>().advance_sim(1.0);
        schedule.run(&mut world);
        max_active = max_active.max(world.resource::<DuctGrid>().awake_count());
        max_edges = max_edges.max(world.resource::<VentStats>().edge_updates);
    }
    let us = t0.elapsed().as_secs_f64() * 1e6 / steps as f64;
    println!(
        "PERF vent 128x128 active: {us:.2} us/step, peak_active={max_active}, peak_edges={max_edges}/step"
    );
    assert!(us < 3000.0, "active duct step too expensive: {us} us");
}

#[test]
fn perf_many_small_networks_stay_asleep() {
    // 4-tile islands every 6 tiles: hundreds of independent networks.
    let size = 129;
    let rows = open_room_map(size);
    let refs: Vec<&str> = rows.iter().map(|s| s.as_str()).collect();
    let map = map_of(&refs);
    let n = (map.width * map.height) as usize;
    let mut thermal = ThermalGrid::new(&map);
    let atmo = AtmosphereGrid::new(&map);
    atmo.sync_all_gas_caps(&mut thermal);
    let mut world = World::new();
    world.insert_resource(map);
    world.insert_resource(thermal);
    world.insert_resource(atmo);
    world.insert_resource(AtmoStats::default());
    world.insert_resource(ship_alive::atmosphere::AtmoSummary::default());
    let mut ducts = DuctGrid::new(size as i32, size as i32);
    for y in (2..size as i32 - 2).step_by(6) {
        for x in (2..size as i32 - 6).step_by(6) {
            for dx in 0..4 {
                ducts.set(TilePos::new(x + dx, y), true);
            }
        }
    }
    world.insert_resource(ducts);
    world.insert_resource(DuctTopology::default());
    world.insert_resource(VentStats::default());
    world.insert_resource(VentSummary::default());
    world.insert_resource(DeviceTiles::sized(n));
    world.insert_resource(SimClock::default());
    let mut schedule = Schedule::default();
    schedule.add_systems(ship_alive::ventilation::ventilation_system);
    drive(&mut world, &mut schedule, 10);
    let nets = world.resource::<DuctTopology>().nets;
    drive(&mut world, &mut schedule, WAKE_STEPS as usize + 10);
    assert!(nets > 300, "expected many networks, got {nets}");
    assert_eq!(
        world.resource::<DuctGrid>().awake_count(),
        0,
        "inactive networks stayed awake"
    );
    let _ = (pressure_vol, GAS_CAP_PER_MOL, Tile::Wall);
}
