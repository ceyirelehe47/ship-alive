# REPORT_ATMOSPHERE — Slice 5: Atmosphere & Pressure

Final code commit: see `Git` at the bottom. All numbers below are from the
actual runs on this branch (scenario outputs and `cargo test -- --nocapture`
perf prints).

## Summary

Air is now a real, per-cell, conserved, flowable resource. A dense
`AtmosphereGrid` stores four gas amounts per tile as the authoritative state;
pressure and every partial pressure are derived from (gas amount × grid
temperature) through an ideal-gas-like relation. Pressure-driven bulk flow
moves whole mixtures with an equilibrium clamp (no overshoot for any dt),
gas carries its sensible heat with it, slow composition diffusion mixes
species, hull breaches vent to an infinite vacuum boundary with a full
per-species + per-energy ledger, and the thermal grid's air heat capacity
now follows the real gas amount. Boundaries are not re-implemented: gas
exchange reuses `airtight::boundary` (closed/opening/closing doors block,
fully open doors and floor/machine tiles exchange). An activity model
mirrors the thermal grid: a uniform sealed ship boots asleep and costs
~0.02 µs/step on a 128×128 map.

## Previous environment model

Through Slice 4 the "air" was an implicit constant: every open tile had a
fixed effective air heat capacity (`AMB_CAP = 24 H/K`) regardless of whether
any gas was there, pressure did not exist, and the only things that cared
about airtightness were heat mixing rates and the compartment graph. That
was correct for the thermal slice (heat conserved through every toggle),
but it could not answer "how much air is here", "where does it flow", or
"what happens when the hull opens" — the questions this slice answers.

## Atmosphere grid

`AtmosphereGrid` (`src/atmosphere.rs`) is a `Resource` with dense
struct-of-arrays per species — `[Vec<f32>; 4]` — plus wake counters, an
awake worklist, and the exterior-vent work list. **No per-cell ECS
entities.** Gas cells are `Floor | Machine | Door` tiles; `Wall`/`BuiltWall`
store no gas. Machine footprints keep a gas volume (same node their device
heat mass couples to — the documented Slice 3 simplification, unchanged).

## Gas species representation

Fixed four slots, dense index (`Species::{O2, Inert, Co2, Pollutant}` =
0..3, `SPECIES` array). No `HashMap<ChemicalId, _>` anywhere. Inert gas is
the single bucket for N₂/Ar/etc. Pollutant has no natural source this slice
— it exists for mixing tests, debug injection and future fire/industry.

## Gas units

Normalized mol-equivalent units: **100 units fill one cell at the standard
atmosphere**. `STANDARD_MOL = 100`, boot composition
`[O2 21.0, inert 78.6, CO2 0.4, pollutant 0.0]`. The "effective cell
volume" is defined by this normalization (uniform for every gas cell; real
cubic meters and per-device displacement are out of scope this slice).
f32 amounts, f64 ledger accumulators. The player never sees these units —
only kPa and %.

## Pressure derivation

```text
P(total_mol, T_°C) = 101.325 kPa × (total_mol / 100) × (T_°C + 273.15) / 294.15
```

Standard boot: 101.3 kPa everywhere at 21 °C. Pressure is never stored —
every consumer (flow, overlay, hover, summary) derives it from the grid +
`ThermalGrid::amb` (the single authoritative temperature; no second
atmosphere-side temperature exists).

## Partial pressure

`P_s = P_total × (amount_s / total)` (0 at vacuum, no division hazards).
Safety bands used by UI/overlay: O₂ partial < 16 kPa unsafe, CO₂ > 3 kPa
high, pollutant > 0.5 kPa polluted, total < 70 kPa "low pressure", < 0.5 kPa
reads as VACUUM.

## Bulk flow model

Per sim step, each unordered 4-orthogonal pair with
`airtight::boundary == Open` (exactly once — same index rule as the thermal
conduction pass, fixed neighbour order → deterministic):

1. Compute the amount `x_eq` that equalizes the pair's pressures exactly:
   `(n_a − x)·T_a = (n_b + x)·T_b` (absolute temperatures).
2. Move `δ = x_eq × min(1, K_BULK·dt)` — a fixed fraction of the
   equalizing amount, so the flux can **never overshoot equilibrium for any
   dt** (a single step of dt=1000 was tested). `K_BULK = 0.12/s`
   (τ ≈ 8 sim s per pair).
3. Species move proportionally to the source cell's composition — never
   just the total.
4. Amounts stay ≥ 0 by construction (δ ≤ x_eq ≤ source amount).

Propagation is spatially local (Gauss–Seidel over the awake snapshot): a
pressure front moves ~1 cell per step, which is what makes door
equalization and decompression fronts visible (scenario C: near-door 62.3
vs far-corner 53.0 kPa inside the same room at t=5).

## Composition diffusion model

After bulk flow, per species: `δ_s = min(1, K_DIFF·dt) × (f_a − f_b) ×
(n_a·n_b)/(n_a+n_b)` with `f` = mole fractions — the same
equilibrium-clamped shape as bulk flow, applied to each species
independently. `K_DIFF = 0.02/s` (τ ≈ 50 s, deliberately much slower than
bulk). It only re-allocates species (total per pair unchanged) and only
through open boundaries. Simplification (documented): diffusion moves no
heat — net mass transfer at equal pressure is negligible.

## Airtight integration

The atmosphere has **no boundary logic of its own**: every neighbour query
calls `airtight::boundary(map, a, b)`. Consequences by construction:
closed / opening / closing doors block all gas exchange; fully open doors
exchange; Hold Open keeps exchanging; Lock Closed stays isolated; heat and
gas always agree on the boundary. Structural compartments are used only as
derived caches (region ids for prints/UI, exposure flags, summary counts) —
never as simulation volume.

## Door tile / portal gas semantics

**Door-cell gas volume model**: a door tile carries a real standard fill
(100 units) exactly like a floor tile. While sealed, that gas sits trapped
(no exchange with either side — it is not a leak path); while fully open,
the cell exchanges with both sides like any open tile. Opening and closing
therefore cannot create or destroy gas — there is no ghost volume to
synchronize, and the trapped fill returns to circulation when the door
opens. This matches the thermal model (the door tile is an ordinary air
node whose conductivity changes) and the compartment portal graph (the door
is a boundary, not a room). Door construction on floor keeps the cell's
gas; door teardown returns it unchanged (test:
`door_tile_keeps_gas_through_build_and_teardown`).

Auto doors are real atmosphere events (scenario D): the seal flips → the
door system wakes the door cell + neighbours the same step → gas flows
while the door is open → the seal flips back → flow stops. No special
"people pass, air doesn't" rule exists.

## Exterior vacuum boundary

Space is not a grid. A gas cell that is 4-adjacent to out-of-bounds is on
the `exposed` work list (recomputed on structural edits only) and vents a
fixed fraction of its gas per second to an infinite zero-pressure sink:
`K_VENT = 0.4/s` (τ ≈ 2.5 s per exposed cell). Neighbours feed the breach
via ordinary bulk flow, which is what makes decompression propagate inward
(scenario G: breach tile 36.4 kPa vs 25-tiles-away 101.4 kPa at t=3).

## Thermal integration

- **Single temperature authority**: the atmosphere reads `ThermalGrid::amb`
  and writes it only for advection mixing; there is no
  `AtmosphereCell.temperature`.
- **Gas heat capacity**: `ThermalGrid` gained a per-tile `gas_cap` written
  by the atmosphere system (`GAS_CAP_PER_MOL = 0.24 H/K` per unit → 100
  units = 24 H/K = the historical `AMB_CAP`, so a pressurized ship keeps
  its Slice 3 thermal balance). Vacuum ⇒ ~0 gas capacity; device thermal
  mass (`ThermalBody.mass`) still adds on top — a depressurized reactor
  room keeps its machinery heat but its air no longer buffers (scenario:
  low-pressure cells heat >5× faster per joule in tests).
- **Advective heat**: bulk flow carries `δ · 0.24 · (T_src + 273.15)` of
  sensible heat; the source keeps its temperature (removing gas does not
  cool it), the destination mixes energies over its new total capacity
  (gas + device mass on the shared node).
- **Vacuum conduction**: the `conduct()` cap guard already returns 0 when
  either side has no capacity, so air↔air exchange stops in vacuum while
  structure↔structure, air↔solid-surface and explicit coolant keep
  working.
- **Scheduling** (deterministic, one fixed step of 1 sim s): SimClock →
  door state/boundary (`door_system`) → atmosphere transport + advection →
  (next step) thermal conduction. The atmosphere runs after the door
  system (a seal flip this step is visible to gas this step — acceptance
  §88) and after the thermal pass (fresh temperatures and device masses);
  its advection writes temperatures the next thermal step consumes. Each
  side is one fixed step behind the other somewhere; that lag is
  deterministic and documented.
- **Cross-wakes**: thermal injection/conduction wakes atmosphere cells at
  the same tiles; atmosphere advection/venting wakes thermal cells. The
  thermal→atmosphere wake uses a coarser epsilon (`THERMAL_WAKE_EPS` =
  0.05 K/step): a slow ship-wide drift toward thermal equilibrium shifts
  pressure by orders of magnitude less than any real flow, so gas cells
  sleep through it (this dropped the starter ship's resting active set
  from ~550 to ~60 cells).

## Gas heat capacity

`cap_tile(i) = 0.24 H/K × gas_units(i)` (+ device mass). Monotone in gas
amount; ~0 at vacuum; device mass unaffected by pressure. Tests:
`gas_heat_capacity_follows_amount_and_vacuum_has_none`,
`low_pressure_gas_heats_faster_than_pressurized`.

## Advective thermal energy

Test `advective_heat_travels_with_the_gas`: a hot over-pressurized cell
flowing into a cold thin one warms the destination while `total_heat`
(Σ cap·T over gas + device + structure) is conserved to 1e-2 H.

## Conservation accounting

`AtmoStats` holds boot totals, per-species vented totals, vented energy
(f64). Two identities hold to float tolerance:

- **Closed system** (no breach, no debug edits): per-species onboard sums
  are bit-stable over 8 sim-hours (scenario A: drift 0.0000 mol).
- **With vacuum**: `boot_s == onboard_s + vented_s` per species (scenario
  G: 47157 + 1143 = 48300 = boot; test `vent_conservation_ledger_per_species`).
- **Thermal**: the Slice 3 invariant upgrades to
  `Δstored = injected − radiator_rejected − vented_gas_energy`;
  `total_heat` accounting makes this exact because vented gas drops its
  `cap·T` contribution (test `vented_gas_carries_its_heat_out`: lost vs
  ledger agree to <1 H over 80767 H vented).

Debug tools book their creations/removals into the same ledger (negative
vented entries) so audits stay meaningful during scenario manipulation.

## Activity / sleep model

Same pattern as the thermal grid: `wake: Vec<u32>` + `awake: Vec<usize>`
worklist; cells sleep after `WAKE_STEPS = 600` quiet steps.
Wake events implemented: door seal flip (door cell + 4 neighbours),
structural edit / breach, debug injection/removal, significant exchange
(> `WAKE_EPS_MOL = 0.01`/pair/step), pressure-relevant thermal change,
exterior exposure appearing. Waking starts at the event, never the whole
ship (scenario Q: door open wakes ≤ 5 cells; the workset then grows only
as far as the actual gradient reaches).

## Simulation Time integration

One atmosphere step per FixedUpdate tick (`dt = SIM_STEP = 1.0`), driven
only by `SimClock`. Pause (dt = 0) freezes flow, diffusion, venting and
wake timers (scenario O: state bit-identical while paused, sim clock
frozen at 300.000). 1×/2×/4× execute the identical step sequence (fixed
dt); scenario P at 1×/2×/4× prints identical species totals
(9618.00 / 35998.80 / 183.20 / 0.00) and pressures within 0.04 kPa — the
residual is the frame-driven scenario driver firing its perturbation ±2
steps apart, not the simulation (unit test
`fixed_steps_are_speed_independent` proves bit-equality for identical step
counts). No second fixed loop, no manual `BASE_SIM_RATE` multiplication.

## UI / Overlay

- `OverlayMode::Atmosphere` (`P` now cycles Off→Power→Thermal→Coolant→
  Compartments→Atmosphere; still one mutually-exclusive mode).
- Main visual = pressure: vacuum near-black → low blue → normal green/cyan
  (95–110 kPa band) → high yellow → extreme red. Composition hazards
  override the tile color (pollutant magenta > high CO₂ orange > low O₂
  pale blue-grey). Doors keep the sealed-red / open-green convention.
  Hovered tile brightens.
- Pooled rendering: one sprite per gas tile, rebuilt only on geometry
  changes; colors refresh on a 10 Hz wall-clock cadence with a quantized
  bucket compare (0.5 kPa bins | hazard | door | hover) — a static ship
  repaints nothing.
- Hover card (overlay active, no entity under cursor): pressure,
  temperature, O₂ amount+partial+%, inert %, CO₂ %, pollutant %,
  compartment number; walls read "solid — no gas volume". No ECS ids.
- SHIP STATUS block: `ATMOSPHERE — Pressure 99–102 kPa / O2 partial
  20.7–21.3 / Gas retained 100% / Exposed 0 compartments`, colored when
  abnormal. Reads only the cached `AtmoSummary` (rebuilt every 30 sim
  steps, never per frame).
- Always-on alerts (no overlay needed): `ATMOSPHERE LOSS — hull breach`
  outranks thermal warnings; `LOW O2 / HIGH CO2 / POLLUTED` follow.
  Exposed regions also get a `VENTING TO SPACE` map label in the overlay.
- Compartments overlay unchanged and separate (structure vs air state).

## Performance

Measured on the dev machine (Ryzen 7 6800H), pure-grid tests:

| case | step cost | active cells |
|---|---|---|
| 128×128 uniform sealed (sleeping) | **0.02 µs/step** | 0 |
| 128×128 pressure-front propagation | **3.66 µs/step** avg (2000 steps) | peak 79, 0 after |
| 113×113 = 256 sealed rooms, one event | ~0 after settle | 0 (inactive rooms never scanned) |

Full app at 4× (scenario P with `SLICE0_PERF`): 4.27 ms average frame,
240.0 sim-s per real-s (the 4× target exactly). Starter ship resting
workset ≈ 60 atmosphere cells (heated FABRICATION room); worst observed
operational set ≈ 200 cells (open door between rooms at different
temperatures + heat path) ≈ single-digit µs/step. Memory: 4 × f32 amounts
+ u32 wake per cell ≈ 20 B/cell.

The sleeping case is the future high-rate (Cruise) foundation: stable
regions carry a wake counter and zero per-step work, so skipping their
re-evaluation is already the steady state, not a special mode.

## Tests

`cargo test`: **169 passed** (142 prior + 27 new in `tests/atmosphere.rs`).
Coverage includes: standard initialization, pressure/partial derivation,
low-total-high-fraction O₂ danger, bulk flow + species co-transport,
no-overshoot at dt=1000, equal-pressure composition mixing, closed door
isolation, gradual open-door equalization (1 step ≠ room average),
pollutant blocked/spread by doors, breach-first decompression ordering,
per-species vent ledger, emergency door isolation + re-open
re-equalization, gas-dependent heat capacity, advective energy
conservation, vented-heat accounting, wall build/teardown conservation,
door build/teardown gas neutrality, pause freeze, fixed-step speed
independence, sleep/wake locality + sealed-room non-propagation, 20k-step
numerical robustness (no NaN/negatives/fraction drift), summary hazards,
and the three performance cases above.

## Acceptance A–T

Full-app scenario driver `SLICE5_SCENARIO` (A–I, O, P, Q) + unit tests:

- **A — stable starter atmosphere**: boot 101.3–101.4 kPa, 48300 mol;
  after 2 sim-hours species drift 0.0000 mol, vented 0, pressure 104.4
  (the ship warms toward its reactor/radiator equilibrium; sealed-room
  pressure follows temperature by design); 8-hour soak 105.6–106.0 kPa,
  active settled 60. No warnings, no auto-leak.
- **B — closed-door isolation**: CARGO at 51.0 vs CREW at 102.1 kPa after
  30 min — zero cross-door exchange; the trapped door cell keeps its fill.
- **C — open-door equalization**: front from the door (near 62.3 / far
  53.0 at t=5), no instant averaging, converging (75.2 vs 97.1 at t=90 —
  big rooms mix slowly by design), conservation exact.
- **D — auto-door transient**: demand opens the door, cargo climbs
  54.0→57.9 while open, flow stops after close (58.1 flat).
- **E — composition mixing**: CARGO 70%→55% O₂ with CO₂ arriving; audit
  exact (48300.0).
- **F — pollutant spreading**: 0.000% in sealed rooms; 0.029% after the
  door opens; conserved.
- **G — decompression**: breach tile first (36.4 vs 101.4 kPa 25 tiles
  away), front moves inward each sample, onboard 47157 + vented 1143 =
  boot 48300, vented heat 80767 H ledgered.
- **H — emergency isolation**: locked door keeps the room at 102.6 kPa
  while the corridor bleeds to 86.7 (2544 mol vented).
- **I — re-open after isolation**: rooms re-approach a shared equilibrium
  (71.8 vs 78.3 with an in-room gradient — flow, not averaging).
- **J — pressure derivation** / **K — O₂ partial**: unit tests
  (`pressure_derivation_matches_definition`,
  `partial_pressure_low_total_high_fraction_still_low`).
- **L — gas thermal advection** / **M — decompression thermal
  accounting** / **N — low-pressure heat capacity**: unit tests listed
  above.
- **O — pause**: state frozen bit-exactly while the renderer keeps
  running.
- **P — speed equivalence**: 1×/2×/4× identical species totals and
  activity, pressures within 0.04 kPa (driver quantization; sim itself
  step-deterministic).
- **Q — sleep/wake**: boot active 0; door-open workset bounded (201);
  after closing, the sealed compartment sleeps again (active 80 = heated
  room only).
- **R — compartments/doors regression**: `SLICE4_SCENARIO=A/B` re-run —
  regions 7 / sealed 7 / exposed 0 / portals 5, phase sequence
  Closed→Opening→Open→Closing→Closed, hauls + door cycles intact.
- **S — thermal/coolant regression**: `SLICE3_SCENARIO=A/B` re-run —
  stable core 33.0 °C (A) and 35.3 °C recovered (B), coolant loop intact,
  no spill; the pressurized gas capacity equals the old constant so Slice
  3 balance is preserved by construction.
- **T — full operations regression**: `SLICE0_SCENARIO=A/B/E` re-run —
  hauls/build/deconstruct/speed controls all at baseline; full test suite
  green.

## Playtest pass 1 (readability)

Normal starter ship, `SLICE5_VIEW=atmosphere` screenshot: interior reads
green/cyan at a glance, five closed doors as red squares, top-bar
`ATMOSPHERE | pressure … kPa | O2 … | retained … | exposed …` summary
present, SHIP STATUS gained a 6-line ATMOSPHERE block without overflowing
the panel (ENV_LINES 26→32), no always-on alert on a healthy ship. Verdict:
the overlay is one `P`-press past the familiar Compartments view and the
hover card answers "what is in this tile" in one look.

## Playtest pass 2 (door equalization)

Scenario C captured mid-front (t≈7): CARGO HOLD visibly bluer/darker than
the green corridor, with a readable gradient from the door into the room —
the cause (opened door after lowering one side) reads directly off the
map. Closing (Lock Closed) re-freezes the split within seconds. Verdict:
causality is直观 — you see where the air is going.

## Playtest pass 3 (decompression & isolation)

Breach run captured at t≈4: near-black cells at the carved hull edge
fading gradually back to green rightward (the pressure front),
`VENTING TO SPACE` label at the exposed region, `ATMOSPHERE LOSS — hull
breach` alert in the top bar with the overlay off-available, Gas retained
dropping in SHIP STATUS. Locking a door between the corridor and a room
visibly stops the dark front at the door line (scenario H numbers: room
102.6 vs corridor 86.7 kPa). Verdict: "关门保空气" is legible in the first
ten seconds of a breach.

## Design assumptions made

- **Standard atmosphere**: 101.325 kPa at 294.15 K (21 °C boot ambient);
  mix 21 / 78.6 / 0.4 / 0 O₂/inert/CO₂/pollutant (≈21.3 kPa O₂ partial).
- **Cell volume**: uniform, defined by `STANDARD_MOL = 100` per gas cell
  (machines and doors included; no per-device displacement).
- **Gas units**: normalized mol (100/cell at standard); player sees kPa/%.
- **Pressure scale**: `P_ref·(n/100)·(T/T_ref)` — linear in n and absolute
  T, exact at the reference point.
- **Bulk-flow coefficient**: `K_BULK = 0.12/s` (τ ≈ 8 s/pair; front ≈ 1
  cell/step) — chosen so a door equalization takes visible seconds and a
  breach empties a corridor in a fraction of a ship-minute.
- **Diffusion coefficient**: `K_DIFF = 0.02/s` (τ ≈ 50 s/pair).
- **Gas heat capacities**: uniform `0.24 H/K` per unit for all species
  (100 units = historical 24 H/K air capacity ⇒ Slice 3 balance holds
  while pressurized).
- **Vacuum flow rule**: exposed cells vent `K_VENT = 0.4/s` of their gas;
  space is an infinite zero-pressure sink, not a grid.
- **Activity thresholds**: wake on exchange > 0.01 mol/pair/step or
  temperature change > 0.05 K/step (thermal-side wake is 0.002 K — the
  coarser gas-side epsilon keeps slow thermal drifts from pinning the
  atmosphere awake); sleep after 600 quiet steps.
- **Structural edits**: wall-building pushes the tile's gas evenly into
  adjacent gas cells (old-adjacency rule); fully-enclosed leftovers stay
  dormant in the tile's arrays and return on teardown; torn walls boot
  near-vacuum and fill by real flow; door/machine conversions keep the
  cell's gas.
- **Epsilon handling**: no amount-snapping — all decay is multiplicative,
  conservation tests tolerate 1e-2 mol over 10⁶ steps.

## Temporary behaviors

- Crew does not breathe; no O₂ consumption, no CO₂ output, no
  suffocation/unconsciousness/death.
- No life support (no O₂ generator / scrubber / electrolyzer / processor /
  tank / refill gameplay).
- No ventilation network (vents, ducts, blowers, fans).
- No fire, ignition, smoke sources, or fire O₂ consumption — pollutant
  enters only via tests/debug.
- No hull-damage gameplay: breaches exist only via the debug carve
  (`SLICE5_TOOLS` F5 / scenarios); no hull HP, weapons, meteors, repairs.
- No pressure force: doors never blow open, crew/items are never pushed.
- No airlock cycling (two-door interlock, pump-down, EVA).
- Fixed four gas species; no arbitrary chemistry.
- The device thermal node remains footprint-coupled (Slice 3 known issue):
  device mass shares the tile temperature; this slice does not further
  entrench it into any atmosphere API.
- Decorative room labels (CARGO HOLD etc.) are not read by the gas system;
  hover/alerts use compartment numbers.

## Known issues

- Diffusion moves no heat (net mass transfer at equal pressure is
  negligible; documented simplification).
- Thermal ↔ atmosphere are each one fixed step behind the other (thermal
  conduction uses last step's gas caps; pressure uses this step's
  temperatures). Deterministic; no contradiction within a step.
- Door equalization between large rooms is diffusion-limited and can take
  ship-minutes to fully converge (by design — "gradual" is the spec), so
  scenario C's t=90 snapshot still shows a 22 kPa room delta.
- The scenario driver fires actions per render frame, so 1×/2×/4×
  equivalence carries a ±2-step action quantization (sim itself is
  step-deterministic).
- Summary/alert granularity is the 30-step cadence (0.5 s at 1×); alerts
  can lag a fresh breach by up to that.
- The 4× app run carries a constant ~29 sim-s startup backlog in
  `SimClock` (pre-existing pump behavior; delivery rate still matches the
  240 steps/s target exactly).
- Crew passing an auto door causes real, small gas exchanges every cycle —
  an operating ship's atmosphere workset near doors never fully sleeps
  (bounded, correct-by-physics).

## Deferred systems

Life Support; crew breathing/survival; ventilation; gas tanks; fire and
smoke sources; hull damage gameplay; airlock cycles; pressure force;
advanced gas chemistry (N₂/Ar split, reactions); humidity/phase change;
device-independent thermal nodes; high-rate (Cruise) batched stepping
(the sleep model is the foundation for it).

## Git

Implementation commit: pushed to `main` after all gates — see the final
report message for the SHA (this file is committed together with the code
it describes).
