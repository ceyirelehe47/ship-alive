# REPORT — Slice 4: Airtight Compartments & Doors

## Summary

Doors are now real runtime devices with state, and the ship's interior is
partitioned into **structural compartments** — a derived cache rebuilt only on
geometry edits — plus a separate **current airtight connectivity** graph that
flips instantly when a door opens or closes. Heat obeys the same boundary:
ambient↔ambient exchange runs at full speed only through fully open doors; a
closed door still seeps slowly (`K_DOOR_SEALED = 1.2` vs `K_AIR_AIR = 22`,
wall surface `0.35`), and toggling a door can never create or destroy heat
(the door tile's capacity never changes — only its conductivity). The starter
ship boots with **five preinstalled auto doors** and **7 sealed structural
compartments** (6 crew-facing rooms + the scenario-C pocket). A new
**Compartments overlay** (`P` cycles to it) tints each compartment a stable
hue, draws closed doors as red barriers / open doors as green links, flashes
exposed-to-space regions, and brightens the hovered compartment.

Final commit: see Git section.

## Current architecture before this slice

- `Tile::Door` existed as a *walkable tile* with no runtime state ("future
  airlock hook"); building a Door wrote `Tile::Door` and nothing else.
- Thermal treated door tiles as plain open air (`solid_cap = 0`, `K_AIR_AIR`
  conduction to all neighbours) — an open doorway between rooms was
  indistinguishable from open floor.
- Movement/A* had no concept of doors beyond walkability; soft avoidance
  would eventually pass through any tile.
- The room labels (CARGO HOLD / …) were (and remain) pure decoration.

## Door model

`airtight::Door` component on each `Building { kind: Door }` entity:

- `mode: DoorMode` — `Auto` (default) / `HoldOpen` / `LockClosed`.
- `phase: DoorPhase` — `Closed / Opening / Open / Closing` (derived label;
  the leaf position is the single continuous `progress: 0..1`).
- `hold_until: f64` — sim-time window that an auto door stays open after the
  last passage demand (anti-flap for multi-crew streams).
- `axis: DoorAxis` — `Ns` (flanked by walls east+west, passage north-south)
  or `Ew` (flanked north+south, passage east-west).
- `cycles: u32` — completed open→close cycles (telemetry; scenario C asserts
  no chatter).

Timings (sim seconds; 60 sim s per real s at 1×):

- Full travel `DOOR_MOVE_SECS = 24` (0.4 real s at 1×) each way.
- Hold window `DOOR_HOLD_SECS = 36` (0.6 real s) after the last demand or
  occupancy; a following crew within ~1.8 tiles keeps the door open.

Crew may step onto the door tile only at `progress ≥ 1.0` (fully open);
anything less is an airtight boundary (`sealed = progress < 1.0` — Opening
and Closing both count as sealed; simple, deterministic).

**Anti-crush** (`#11`): the leaf never closes onto a crew member — closing
freezes while anyone stands on the tile or is about to step in (their
`path[0]`). A locked door waits for the occupant to leave, then closes.

**Demand flow**: the movement system registers passage demands into
`DoorDemand` (consumed and cleared by `door_system` next step). Waiting for a
door is *not* congestion: every soft-avoidance clock (`blocked_for`,
`stuck_for`, sidestep cooldown, pass-through) stays frozen while a crew waits
(`#52`).

## Door modes

| Mode | Leaf | Walkable | Airtight |
| --- | --- | --- | --- |
| Auto | closed when idle; opens on demand, holds 36 sim s | yes | sealed unless fully open |
| Hold Open | held fully open | yes | open — air linked |
| Lock Closed | closed (waits for occupants to clear) | **no** | sealed |

`Action::SetDoorMode` (door selection panel buttons) switches modes; current
mode highlighted. No permissions system — Lock Closed is a global player
control.

## Door orientation / placement rule

`airtight::door_axis(map, pos)`:

- `Ns` iff east+west neighbours are wall (hull or built — machines and OOB
  count as wall) **and** north+south are standable interior.
- `Ew` iff north+south are wall and east+west standable.
- Otherwise **no valid orientation** → `PlacementError::BadDoorSpot`
  ("doors need a one-tile wall opening"). This covers both the open-hall
  magic-cube case (`#19`) and the ambiguous cross-of-walls case (`#18` —
  chosen option: reject with a ghost reason, no orientation picker).
- Side-by-side doors are impossible by the same rule (a door's lateral
  neighbour must be wall).

The build ghost shows the inferred axis ("Door (N-S) — 2 parts") when legal.
The strict 8-way no-corner-cut rule already prevents diagonal entry/exit
through a door frame (wall-flanked leaf ⇒ side cells blocked); covered by
`no_diagonal_cut_through_door_frame_corner`.

## Structural compartment model

`airtight::Compartments` resource — pure derived cache:

- Dense per-tile `id: Vec<u16>` over `Floor` + `Machine` tiles (machine
  footprints carry room air volume, matching the Slice 3 thermal model).
  Door tiles are **portals, not volume**. `NO_REGION` for walls/doors/OOB.
- `regions: Vec<RegionInfo>` — cell count, centroid, bounding box,
  `exposed` (a member tile 4-adjacent to out-of-bounds → would vent to
  space; identification only this slice, `#23`/`#25`).
- `doors: Vec<PortalDoor>` — one per door tile: entity, pos, axis, `side_a`
  (north/west region) and `side_b` (south/east region); either may be
  `NO_REGION` when the door abuts structure.
- Flood fill is 4-connected, scan-order region ids ⇒ same topology gives the
  same ids and the same overlay colors (`#34`); only geometry edits renumber.

No per-tile ECS entities (`#7`) — dense arrays only, per the grid/ECS hybrid.

## Airtight connectivity model

- `air_group: Vec<u16>` maps each region to a **current air group**: a tiny
  union-find across portals whose door is fully open. Two regions are
  environmentally connected iff same group.
- Recomputed **only** when a door's seal actually flips (or on structural
  rebuild): O(regions + portals), never a tile scan (`#47`/`#48`).
- Unified boundary semantics for every environmental system (`#28`):
  `airtight::boundary(map, a, b) -> Boundary::{Blocked, Open}` for
  orthogonally adjacent tiles — wall/door-sealed/OOB ⇒ Blocked, open door or
  same-room air ⇒ Open. Future Atmosphere/Smoke/Pressure/Fire extend this
  with permeability instead of writing their own `if Wall / if Door` checks.
  Thermal uses the same underlying seal flag directly (below).

## Derived-cache architecture

```
ShipMap::version (geometry edits only)          door seal flips
        │                                             │
        ▼                                             ▼
compartment_sync_system (Update/Sync)          door_system (FixedUpdate/Jobs)
  rebuild flood fill + portals                   recompute_air (union-find)
        │                                             │
        └──────────► Compartments resource ◄──────────┘
                         (regions, portals, air groups, door tallies)
```

- **Geometry changed** (build/demo wall or door): `map.version` bumps;
  the sync system rebuilds the partition next frame.
- **Door state changed**: only the connectivity edge flips. Scenario O
  flapped a door's mode 24 times: `rebuilds = 1`, `air_recomputes = 25`.
- Stable world: scenario N ran 90 old-s at 4× — `rebuilds = 1` (boot),
  `air_recomputes = 1`. A stable frame costs one integer compare.

Runtime door state is mirrored into `ShipMap`'s dense `DoorTileState` grid
(`open`, `locked`) by `door_system` every step, so pathfinding and movement
read it with zero ECS queries. Runtime state deliberately does **not** bump
`version`.

## Thermal integration

- A door tile stays an **ordinary air node with constant capacity** (AMB_CAP;
  no device mass). Sealing flips only a per-tile `sealed` flag in
  `ThermalGrid`, which switches the neighbour conduction coefficient:
  `K_AIR_AIR = 22` ↔ `K_DOOR_SEALED = 1.2`.
- Consequences: closed door = slow two-sided seep (`#30`, airtight ≠
  adiabatic; ~3.4× a wall surface, ~18× slower than open air); open door =
  fast direct ambient mixing (`#29`); a toggle changes **no temperature and
  no capacity**, so total heat is conserved across the toggle *exactly* —
  `door_toggle_conserves_heat_exactly` asserts delta < 1e-3 both ways (`#31`).
- Sealing/unsealing wakes the door tile; conduction then naturally wakes
  exactly the tiles the exchange reaches (`#49` — no whole-ship wake).
- Machine footprint tiles remain room air volume (known Slice 3
  simplification, unchanged per `#32`).

Measured on the starter ship (scenarios J/K/L, 4×, production heat on):

| | FAB room | corridor | ΔT |
| --- | --- | --- | --- |
| J door closed 45→90 old-s | 33.5 °C | 29.9 °C | 3.6 (growing) |
| K door held open | 33.5 °C | 31.4 °C | 2.1 (converging) |
| L re-closed | 33.9 °C | 29.7 °C | 4.1 (diverging again, no resets) |

Precise isolation rates are unit-tested (`closed_door_blocks_fast_ambient_mixing`).

## Pathfinding integration

- `ShipMap::is_walkable`: floor always; door iff not locked. Locked doors
  are walls to A*, job reachability, drop tiles and interaction tiles — all
  existing call sites inherited the rule for free.
- `find_path` goal check uses `is_walkable`; the **start** uses
  `is_standable` (floor or any door) so a crew caught inside a door tile
  when it locks can still path *out*.
- Movement: stale plans through a just-locked door are dropped (task system
  re-paths → unreachable → existing `NoPathUntil`/rescan cooldowns, so no
  claim/fail pump, `#51`); closed-but-unlocked doors get a passage demand and
  a clock-frozen wait (`#52`).
- No door traversal cost was added (`#15` optional); the opening delay is
  the friction. Job distance metrics therefore stay unchanged.
- 8-way regression: all path8 tests green, plus the new door-corner test.

## Simulation Time integration

- Leaf travel, hold windows and demand consumption all advance in sim steps
  (`clock.dt()`); pause stops FixedUpdate entirely, so progress and timers
  freeze while the UI stays responsive (`#53`).
- `door_rate_depends_only_on_sim_time_integral`: stepping 24×1 s vs 6×4 s of
  sim time reaches the same state; scenario P ran the full app at 1×/2×/4×
  to sim t=6000 — identical door phases, regions and air groups (`#54`).

## UI / Overlay

- **Compartments overlay** (4th `P` press; single `OverlayMode` resource —
  no new toggles, `#33`): pooled per-tile sprites tinted per compartment
  (stable hue per region id; write-if-changed buckets — 0 extra frame cost),
  closed doors = solid red barriers, open doors = green links (`#35`),
  exposed regions flash warning orange-red with an "EXPOSED TO SPACE" label
  (`#34`), and the hovered compartment brightens (`#34`).
- **SHIP STATUS** gains a compact block (`#36`):
  `COMPARTMENTS — 7 structural | 7 sealed | 0 exposed` /
  `Doors: 5 closed / 0 open | air regions 6` (air regions only shown when
  open doors actually merge groups). The env pane's line pool was enlarged
  (16→26) — the STORAGE/PRODUCTION blocks had silently been truncating.
- **Door selection panel** (`#37`): state/mode, passage axis with flank
  summary, sides ("Compartment 5 | sealed | Compartment 6" ↔ "<-air-linked"),
  airtight status, plus Auto / Hold Open / Lock Closed buttons (current mode
  highlighted) and Deconstruct.
- **Normal view** (`#38`): the leaf squashes along its wall line as it opens
  (Ns shrinks X, Ew shrinks Y — verified in screenshots), open doors render
  as a faded sliver, locked doors take a red tint; door tooltip shows
  "Closed (Auto) — airtight".
- Ghost: legal door placements preview the axis; illegal ones show
  "doors need a one-tile wall opening".

## Performance

- `tests/airtight.rs` stress (debug build): 128×128 synthetic ship, 1024
  compartments, 992 door portals — structural rebuild **0.92 ms**; air
  recompute (union-find over the portal graph) **59 µs**; worst-case door
  step (240 doors, half demanding every step, 200 steps) **39 µs/step**.
- Full app (`SLICE0_PERF`): 1× ≈ 5.1 ms/frame, 4× ≈ 5.1 ms/frame,
  sim rate exactly 240 sim-s/s at 4×; with the Compartments overlay active
  at 4× ≈ 4.8–5.2 ms/frame (bucket-compare writes only). vs. the pre-slice
  ≈ 4.6 ms — the airtight stack costs ~0.5 ms/frame at debug settings.
- Door toggles never rebuild structure (scenario O: 24 mode flaps,
  `rebuilds = 1`); stable topology is one integer compare per frame
  (scenario N).

## Tests

142 total pass (113 pre-existing + 29 new in `tests/airtight.rs`):
compartment detection, sealed region, exposed region (synthetic hull gap),
structural split, structural merge, portal creation/removal, air-group
connectivity, boundary query, auto-door full cycle, Hold Open, Lock Closed,
locked-door closing waits for occupant, multi-crew stream (no flap), movement
demands + waits (clocks frozen), stale-plan drop on lock, pause freeze,
sim-time-integral rate equivalence, path through auto door, no path through
locked door (but can path *out* of one), diagonal door-frame corner rule,
orientation inference (hall/cross rejected), closed-door isolation rate,
open-door propagation + wake, exact conservation across toggle, stable
topology cache, door-toggle cache behavior, 128×128 rebuild/step perf,
64×64 door-state step perf, starter-ship boot partition.

## Acceptance A–S

| | Scenario / evidence | Result |
| --- | --- | --- |
| A | starter compartments | 7 regions, all sealed, 0 exposed; hauls flowing (23 done, 27 stored) — identical throughput to pre-slice |
| B | auto door passage | phase seq `Closed→Opening→Open→Closing→Closed`, no stall |
| C | multiple crew drain | 24 hauls through one door, 12 cycles (closures only between separated trips; dense streams batch — unit-proven), ends Closed |
| D | Hold Open | open, stays open, air groups merge (5/7), traffic continues |
| E | Lock Closed | no pathing through, 0 crew inside, unreachable items cooldown-bounded (9 retries over 60 old-s — no pump), no soft-avoidance breach |
| F | structural split | wall both storage gaps → 8 regions, rebuilds 3 |
| G | structural merge | tear one wall → 7 regions |
| H | build door | new door at (32,9): N-S axis, sealed, 8 passage cycles, hauls continue |
| I | door demolition | portal gone (6→5), regions merged, `boundary()` open — no ghost |
| J | thermal isolation | ΔT grows 3.6 (no fast mixing) |
| K | thermal connection | door open → corridor converges (ΔT 2.1) |
| L | re-close | exchange stops, ΔT diverges again, no temp resets; conservation unit-exact |
| M | exterior exposure | synthetic-map unit tests: border-gap region = EXPOSED, sealed otherwise |
| N | stable cache | 90 old-s at 4×: rebuilds = 1, air_recomputes = 1 |
| O | door toggle perf | 24 mode flaps: rebuilds = 1, air_recomputes = 25, 39 µs/step worst case |
| P | time equivalence | 1×/2×/4× to t=6000: identical doors/regions/groups |
| Q | operations regression | SLICE0 A–L, P1, P2, M all pass (A: same 23 hauls / 39 stored as pre-slice) |
| R | power/thermal/coolant | SLICE2 A,C,D,F,G + SLICE3 A,B,R pass (core 31.6–35.4 °C Normal, coolant loop intact) |
| S | 8-way regression | path8 suite green + new door-corner test |

## Playtest pass 1 — door readability

Screenshot + vision-model verification on the live game: doors visibly change
between closed (full leaf), open (retracted sliver, faded) and locked
(red-tinted); the Compartments overlay was found via `P` on the first try and
read correctly ("distinct pastel tints per room, red door tiles, HUD:
COMPARTMENTS 7 structural | 7 sealed | 0 exposed, Doors 5 closed / 0 open").
Auto behavior looked natural (no visible stall; haul pace unchanged). No
issues found → no fixes needed beyond the tint-visibility bump made during
the pass (region alpha 0.34→0.48).

## Playtest pass 2 — remodel

Scripted as scenarios F→G→H→I with screenshots: building walls visibly splits
compartments (overlay recount), a door placed in the opening shows its axis
on the ghost and seals when closed, Hold Open merges the air groups (panel
shows "<-air-linked"), Lock Closed reddens the door and blocks traffic,
tearing the door out merges the rooms permanently. The physical feedback
chain reads clearly through the overlay + panel.

## Playtest pass 3 — operations + thermal

Scenarios J/K/L + regression battery: continuous production while doors
cycle; FABRICATION heats with its door closed and the corridor stays cool;
opening the door starts the convergence; re-closing stops it without
resetting anything. Logistics kept working throughout (haul throughput
identical to the pre-door baseline; door congestion none — waiting crew never
escalated to pass-through). Power/cooling loops unaffected.

## Design assumptions made

- **Door open duration 24 sim s, close 24 sim s, auto-close hold 36 sim s**
  after the last demand/occupancy (0.4/0.4/0.6 real s at 1×).
- **Sealed-until-fully-open**: Opening/Closing count as airtight; only
  `progress = 1.0` connects air and admits crew.
- **Orientation inference** from flanking walls (no orientation picker);
  ambiguous spots are rejected with a ghost reason. 1×1 doors only — a
  door's lateral neighbours must be wall.
- **No door traversal cost** in A*/job distance (optional per brief); the
  opening delay is the friction.
- **Preinstalled doors**: (6,6) CARGO HOLD, (16,6) CREW QUARTERS, (28,6)
  ORE BAY, (5,9) PARTS ROOM, (17,9) FABRICATION — each is its room's only
  opening. STORAGE intentionally keeps both corridor gaps open, so the
  starter ship demonstrates one large multi-opening region.
- **Boot partition = 7 compartments** (6 rooms + the sealed scenario-C
  pocket at (3,16)); all sealed, none exposed.
- **Machine footprints are room air volume** (not boundaries) — consistent
  with the Slice 3 thermal node model; a machine wall is not airtight.
- Crew can path *out of* a door tile they occupy even when it locks.
- Items may be dropped on open (unlocked) door tiles; doors ignore items.
- Door mode changes take effect on the next door step (frame-level latency).
- Overlay region colors: hue = 47° × region id, s 0.75, l 0.62, α 0.48
  (0.68 hovered); exposed = warning red-orange.

## Temporary behaviors

- **Doors currently operate without power** (no actuator demand, no
  fail-open/fail-closed design yet).
- No door permissions / ownership / access levels.
- No gas, no pressure, no decompression, no ventilation, no fire, no
  airlock interlock (two doors are just two doors), no hull damage.
- Structural compartments store no authoritative environment — they are a
  cache; air connectivity is a separate derived graph.
- The current device thermal node remains footprint-coupled (Slice 3
  simplification, deliberately not rewritten).
- The env pane line pool is a fixed 26 lines (content-fitted, not scrolling).
- `SLICE4_SCENARIO` / `SLICE4_SPEED` are dev-driver env vars like the
  earlier slices' ones.

## Known issues

- A locked door whose tile holds a ground item: haul jobs to that item fail
  with the standard unreachable cooldown (bounded, but the item is stranded
  until unlock — acceptable, matches wall behavior).
- Door visuals reuse the Slice 0 door art with squash/tint; no dedicated
  open/locked sprites yet (readable, but art polish is open).
- `M` layout A/B: absolute time-to-parts roughly doubled after the rework
  phase because the fresh ore spawns route cargo→corridor→FAB through two
  doors; haul *distance* per part still improved (206 vs 273). Scenario A
  throughput is unchanged, so this is door friction on a long route, not a
  pathing bug.
- Region ids renumber after geometry edits (scan order), so overlay hues can
  shift when walls change — same topology keeps colors stable.

## Deferred systems

Atmosphere · Pressure · Gas transport · Ventilation · Life Support · Fire ·
Breach/Hull damage · Airlock cycling · Door power · Door permissions ·
independent device thermal node · room renaming/zones.

## Git

- Branch `main`, direct push (no force).
- Final SHA: `f7eb589` (full: bd7c11f26ff8a36fd40a3b7e408cb35efc0a3872)
