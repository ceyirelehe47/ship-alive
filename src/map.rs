//! The starter ship: a fixed hand-authored tile layout stored as a dense grid.
//!
//! Tiles are *not* ECS entities — the grid is dense spatial data owned by the
//! `ShipMap` resource. Dynamic things (crew, items, racks) are ECS entities
//! that reference the grid through `TilePos`.
//!
//! The layout deliberately mixes wide corridors with one-tile-wide doors so
//! the playtest can show whether single-tile choke points are a problem.

use bevy::prelude::*;

/// Legend:
///  `#` hull wall       `.` floor
///  `S` storage rack    `C` crew spawn
///  `P` parts rack (4 machinery parts)   `O` ore rack (4 asteroid ore)
///  `F` fabricator origin (2x2 machine — all four tiles carry the char)
///  `R` reactor origin (2x2 machine — all four tiles carry the char)
///  `c` underfloor power cable
///  `p` underfloor coolant pipe
///  `K` coolant pump (implies a pipe underneath)
///  `W` coolant reservoir (implies a pipe underneath)
///  `H` heat exchanger (implies a pipe underneath)
///  `Z` radiator (implies a pipe underneath; must be hull-adjacent)
///  `D` preinstalled auto door (Slice 4 airtight boundary)
///  `1` crate item      `2` ore item      `3` machinery part item
///  `X` item inside a sealed pocket (permanently unreachable — scenario C)
///
/// Slice 1 layout notes: the ore bay (top right) and the storage racks
/// (bottom right) are deliberately far from the fabricator (FABRICATION,
/// bottom middle) so a bad supply layout is visible and worth optimizing.
///
/// Slice 2 notes: a Starter Reactor sits in FABRICATION's bottom-left corner
/// feeding a short pre-wired cable run to the fabricator — a working ship on
/// day one with plenty of room to rewire.
///
/// Slice 3 notes: a pre-installed coolant loop shares the reactor corner:
/// heat exchanger `H` beside the reactor core, pump `K`, two hull-mounted
/// radiators `Z` along the bottom wall, and a reservoir `W`, all connected
/// by a pipe ring — the ship boots thermally stable.
pub const MAP_LAYOUT: [&str; 19] = [
    "####################################",
    "#.3..1.....#..........#...2...2....#",
    "#..1....1..#....C.....#....2...2...#",
    "#.3....3...#....C.....#..........2.#",
    "#..1....1..#....C.....#...2...2....#",
    "#........1.#....C.....#........2...#",
    "######D#########D###########D#######",
    "#..................................#",
    "#.............3............3.......#",
    "#####D###########D#########.####.###",
    "#..........#..........#....SPS.....#",
    "#..........#..........#....SPS.....#",
    "#..........#...FF.....#............#",
    "#..3.......#...FF.....#....SO......#",
    "#....3.3...#...c......#....SO......#",
    "#.###......#3..c......#............#",
    "#.#X#......#RRccpppppp#............#",
    "#.###......#RRHKpZpZpW#............#",
    "####################################",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tile {
    /// Hull wall (part of the fixed ship shell — never player-built).
    Wall,
    Floor,
    /// Player-built interior wall (blocks movement, deconstructable).
    BuiltWall,
    /// Door tile (walkable while unlocked; a runtime `Door` device entity on
    /// it owns the open/closed state — see `airtight.rs`).
    Door,
    /// Tile occupied by a multi-tile machine footprint (blocks movement).
    Machine,
}

/// Runtime state of one door tile, mirrored from the ECS `Door` component so
/// pathfinding and movement can read it from the pure map with no queries.
/// `open` 0 = closed .. 1 = fully open; `locked` = Lock Closed mode.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct DoorTileState {
    pub open: f32,
    pub locked: bool,
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TilePos {
    pub x: i32,
    pub y: i32,
}

impl TilePos {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// Dense grid of the ship interior. Rows are stored top-to-bottom.
#[derive(Resource, Clone)]
pub struct ShipMap {
    pub width: i32,
    pub height: i32,
    tiles: Vec<Tile>,
    /// Door runtime state per tile (meaningful where `tiles` is `Door`).
    /// Written by the door system only — never bumps `version` (door state
    /// is runtime data, not geometry).
    doors: Vec<DoorTileState>,
    /// Bumped on every `set_tile` so overlays can detect tile-set changes
    /// (walls built/torn) without re-scanning the grid.
    pub version: u64,
}

impl ShipMap {
    /// Parse the layout, returning the map plus the spawn requests it contains.
    pub fn from_layout(layout: &[&str]) -> (Self, Vec<SpawnReq>) {
        let height = layout.len() as i32;
        let width = layout.first().map(|r| r.len()).unwrap_or(0) as i32;
        assert!(
            layout.iter().all(|r| r.len() as i32 == width),
            "map rows must all have equal length"
        );
        let mut tiles = Vec::with_capacity((width * height) as usize);
        let mut spawns = Vec::new();
        // Origins of 2x2 fabricators already registered (the map char repeats
        // on all four footprint tiles; only the first spawns an entity).
        let mut fab_origins: Vec<(i32, i32)> = Vec::new();
        for (y, row) in layout.iter().enumerate() {
            for (x, ch) in row.chars().enumerate() {
                let pos = TilePos::new(x as i32, y as i32);
                match ch {
                    '#' => tiles.push(Tile::Wall),
                    'F' => {
                        tiles.push(Tile::Machine);
                        let covered = fab_origins.iter().any(|&(ox, oy)| {
                            x as i32 >= ox
                                && x as i32 <= ox + 1
                                && y as i32 >= oy
                                && y as i32 <= oy + 1
                        });
                        if !covered {
                            fab_origins.push((x as i32, y as i32));
                            spawns.push(SpawnReq::Fabricator { pos });
                        }
                    }
                    'R' => {
                        tiles.push(Tile::Machine);
                        let covered = fab_origins.iter().any(|&(ox, oy)| {
                            x as i32 >= ox
                                && x as i32 <= ox + 1
                                && y as i32 >= oy
                                && y as i32 <= oy + 1
                        });
                        if !covered {
                            fab_origins.push((x as i32, y as i32));
                            spawns.push(SpawnReq::Reactor { pos });
                        }
                    }
                    'c' => {
                        tiles.push(Tile::Floor);
                        spawns.push(SpawnReq::Cable { pos });
                    }
                    'p' => {
                        tiles.push(Tile::Floor);
                        spawns.push(SpawnReq::Pipe { pos });
                    }
                    'K' => {
                        tiles.push(Tile::Floor);
                        spawns.push(SpawnReq::Pipe { pos });
                        spawns.push(SpawnReq::Pump { pos });
                    }
                    'D' => {
                        tiles.push(Tile::Door);
                        spawns.push(SpawnReq::Door { pos });
                    }
                    'W' => {
                        tiles.push(Tile::Floor);
                        spawns.push(SpawnReq::Pipe { pos });
                        spawns.push(SpawnReq::Reservoir { pos });
                    }
                    'H' => {
                        tiles.push(Tile::Floor);
                        spawns.push(SpawnReq::Pipe { pos });
                        spawns.push(SpawnReq::HeatExchanger { pos });
                    }
                    'Z' => {
                        tiles.push(Tile::Floor);
                        spawns.push(SpawnReq::Pipe { pos });
                        spawns.push(SpawnReq::Radiator { pos });
                    }
                    '.' | 'C' | 'S' | 'P' | 'O' | 'X' => tiles.push(Tile::Floor),
                    d @ ('1' | '2' | '3') => {
                        tiles.push(Tile::Floor);
                        spawns.push(SpawnReq::Item {
                            pos,
                            kind: d.into(),
                        });
                    }
                    _ => panic!("unknown map char {ch:?} at {pos:?}"),
                }
                match ch {
                    'C' => spawns.push(SpawnReq::Crew { pos }),
                    'S' => spawns.push(SpawnReq::Rack { pos, fill: None }),
                    'P' => spawns.push(SpawnReq::Rack {
                        pos,
                        fill: Some((crate::items::ItemKind::Part, 4)),
                    }),
                    'O' => spawns.push(SpawnReq::Rack {
                        pos,
                        fill: Some((crate::items::ItemKind::Ore, 4)),
                    }),
                    'X' => spawns.push(SpawnReq::Item {
                        pos,
                        kind: crate::items::ItemKind::Crate,
                    }),
                    _ => {}
                }
            }
        }
        (
            Self {
                width,
                height,
                tiles,
                doors: vec![DoorTileState::default(); (width * height) as usize],
                version: 0,
            },
            spawns,
        )
    }

    pub fn in_bounds(&self, p: TilePos) -> bool {
        p.x >= 0 && p.y >= 0 && p.x < self.width && p.y < self.height
    }

    pub fn tile(&self, p: TilePos) -> Option<Tile> {
        if self.in_bounds(p) {
            Some(self.tiles[(p.y * self.width + p.x) as usize])
        } else {
            None
        }
    }

    pub fn is_walkable(&self, p: TilePos) -> bool {
        match self.tile(p) {
            Some(Tile::Floor) => true,
            // A locked door is a wall to everyone (until unlocked); an auto
            // or held-open door is walkable even while still closed — crew
            // open it by demanding passage.
            Some(Tile::Door) => !self.doors[(p.y * self.width + p.x) as usize].locked,
            _ => false,
        }
    }

    /// Tiles a crew may *stand* on: floor plus any door — including a locked
    /// one, because whoever stands inside it when it locks must still be able
    /// to walk out (pathfinding out of the tile they occupy is legal).
    pub fn is_standable(&self, p: TilePos) -> bool {
        matches!(self.tile(p), Some(Tile::Floor) | Some(Tile::Door))
    }

    /// Door runtime state at `p` (Some only where the tile is a door).
    pub fn door_state(&self, p: TilePos) -> Option<DoorTileState> {
        if self.tile(p) == Some(Tile::Door) {
            Some(self.doors[(p.y * self.width + p.x) as usize])
        } else {
            None
        }
    }

    /// Write the runtime state of the door tile at `p`. Runtime state is not
    /// geometry: this deliberately does NOT bump `version`, so compartment
    /// caches only rebuild on real structural edits.
    pub fn set_door_state(&mut self, p: TilePos, state: DoorTileState) {
        assert!(
            self.tile(p) == Some(Tile::Door),
            "set_door_state on a non-door tile at {p:?}"
        );
        self.doors[(p.y * self.width + p.x) as usize] = state;
    }

    /// Overwrite one tile (used when buildings complete or are torn down).
    /// Slice 1 only ever writes floor-side tiles; hull walls are never touched.
    pub fn set_tile(&mut self, p: TilePos, tile: Tile) {
        assert!(self.in_bounds(p), "set_tile out of bounds at {p:?}");
        self.tiles[(p.y * self.width + p.x) as usize] = tile;
        // New doors boot closed + unlocked; any other tile clears the slot.
        self.doors[(p.y * self.width + p.x) as usize] = DoorTileState::default();
        self.version += 1;
    }

    /// World-space center of a tile (row 0 renders at the top).
    pub fn world_pos(&self, p: TilePos) -> Vec2 {
        Vec2::new(
            (p.x as f32 + 0.5) * crate::TILE,
            -((p.y as f32 + 0.5) * crate::TILE),
        )
    }

    /// Nearest tile to a world-space position.
    pub fn tile_at_world(&self, w: Vec2) -> Option<TilePos> {
        let x = (w.x / crate::TILE).floor() as i32;
        let y = (-w.y / crate::TILE).floor() as i32;
        if self.in_bounds(TilePos::new(x, y)) {
            Some(TilePos::new(x, y))
        } else {
            None
        }
    }

    pub fn iter_tiles(&self) -> impl Iterator<Item = (TilePos, Tile)> + '_ {
        let w = self.width;
        self.tiles.iter().enumerate().map(move |(i, t)| {
            let i = i as i32;
            (TilePos::new(i % w, i / w), *t)
        })
    }
}

/// Things the hand-authored layout asks the game to spawn.
#[derive(Clone, Copy, Debug)]
pub enum SpawnReq {
    Crew {
        pos: TilePos,
    },
    Rack {
        pos: TilePos,
        /// Pre-stocked kind + amount.
        fill: Option<(crate::items::ItemKind, u32)>,
    },
    /// 2x2 fabricator with its top-left tile at `pos`.
    Fabricator {
        pos: TilePos,
    },
    /// 2x2 starter reactor with its top-left tile at `pos`.
    Reactor {
        pos: TilePos,
    },
    /// Underfloor power cable on this tile.
    Cable {
        pos: TilePos,
    },
    /// Underfloor coolant pipe on this tile.
    Pipe {
        pos: TilePos,
    },
    /// Coolant hardware standing on the pipe at `pos`.
    Pump {
        pos: TilePos,
    },
    /// Preinstalled auto door on the tile at `pos` (Slice 4).
    Door {
        pos: TilePos,
    },
    Reservoir {
        pos: TilePos,
    },
    HeatExchanger {
        pos: TilePos,
    },
    Radiator {
        pos: TilePos,
    },
    Item {
        pos: TilePos,
        kind: crate::items::ItemKind,
    },
}

impl From<char> for crate::items::ItemKind {
    fn from(c: char) -> Self {
        match c {
            '2' => crate::items::ItemKind::Ore,
            '3' => crate::items::ItemKind::Part,
            _ => crate::items::ItemKind::Crate,
        }
    }
}

/// Find a free floor tile near `around` for dropping an item.
/// Prefers `around` itself, then scans outward in a small square.
pub fn find_drop_tile(
    map: &ShipMap,
    around: TilePos,
    racks: &[(TilePos, Entity)],
) -> Option<TilePos> {
    for dy in 0..=2 {
        for dx in 0..=2 {
            for p in [
                TilePos::new(around.x + dx, around.y + dy),
                TilePos::new(around.x - dx, around.y + dy),
                TilePos::new(around.x + dx, around.y - dy),
                TilePos::new(around.x - dx, around.y - dy),
            ] {
                if map.is_walkable(p) && !racks.iter().any(|&(rp, _)| rp == p) {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Like [`find_drop_tile`] but takes a plain list of tiles to avoid (used when
/// spawning several refund items at once).
pub fn find_drop_tile_ext(map: &ShipMap, around: TilePos, occupied: &[TilePos]) -> Option<TilePos> {
    for dy in 0..=3 {
        for dx in 0..=3 {
            for p in [
                TilePos::new(around.x + dx, around.y + dy),
                TilePos::new(around.x - dx, around.y + dy),
                TilePos::new(around.x + dx, around.y - dy),
                TilePos::new(around.x - dx, around.y - dy),
            ] {
                if map.is_walkable(p) && !occupied.contains(&p) {
                    return Some(p);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_map() -> ShipMap {
        ShipMap::from_layout(&["#####", "#...#", "#.#.#", "#...#", "#####"]).0
    }

    #[test]
    fn layout_parses_with_valid_rows() {
        let (map, spawns) = ShipMap::from_layout(&MAP_LAYOUT);
        assert_eq!(map.width, 36);
        assert_eq!(map.height, 19);
        assert_eq!(
            spawns
                .iter()
                .filter(|s| matches!(s, SpawnReq::Crew { .. }))
                .count(),
            4
        );
        assert_eq!(
            spawns
                .iter()
                .filter(|s| matches!(s, SpawnReq::Rack { .. }))
                .count(),
            10
        );
        assert_eq!(
            spawns
                .iter()
                .filter(|s| matches!(s, SpawnReq::Fabricator { .. }))
                .count(),
            1
        );
        assert_eq!(
            spawns
                .iter()
                .filter(|s| matches!(s, SpawnReq::Reactor { .. }))
                .count(),
            1
        );
        assert_eq!(
            spawns
                .iter()
                .filter(|s| matches!(s, SpawnReq::Cable { .. }))
                .count(),
            4
        );
        assert_eq!(
            spawns
                .iter()
                .filter(|s| matches!(s, SpawnReq::Pipe { .. }))
                .count(),
            14,
            "9 plain pipes + 5 device tiles"
        );
        assert_eq!(
            spawns
                .iter()
                .filter(|s| matches!(s, SpawnReq::Pump { .. }))
                .count(),
            1
        );
        assert_eq!(
            spawns
                .iter()
                .filter(|s| matches!(s, SpawnReq::Reservoir { .. }))
                .count(),
            1
        );
        assert_eq!(
            spawns
                .iter()
                .filter(|s| matches!(s, SpawnReq::HeatExchanger { .. }))
                .count(),
            1
        );
        assert_eq!(
            spawns
                .iter()
                .filter(|s| matches!(s, SpawnReq::Radiator { .. }))
                .count(),
            2
        );
        assert_eq!(
            spawns
                .iter()
                .filter(|s| matches!(s, SpawnReq::Door { .. }))
                .count(),
            5,
            "five preinstalled auto doors (6 sealed compartments at boot)"
        );
        assert_eq!(
            spawns
                .iter()
                .filter(|s| matches!(s, SpawnReq::Item { .. }))
                .count(),
            24
        );
    }

    #[test]
    fn walkability() {
        let map = test_map();
        assert!(map.is_walkable(TilePos::new(1, 1)));
        assert!(!map.is_walkable(TilePos::new(0, 0)));
        assert!(!map.is_walkable(TilePos::new(2, 2)));
        assert!(!map.is_walkable(TilePos::new(-1, 1)));
        assert!(!map.is_walkable(TilePos::new(9, 9)));
    }

    #[test]
    fn world_roundtrip() {
        let map = test_map();
        let p = TilePos::new(2, 3);
        let w = map.world_pos(p);
        assert_eq!(map.tile_at_world(w), Some(p));
        // slightly off-center still maps to the same tile
        assert_eq!(map.tile_at_world(w + Vec2::new(5.0, -5.0)), Some(p));
    }
}
