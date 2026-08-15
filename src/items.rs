//! Ground items: cargo the player wants hauled into storage.

use bevy::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ItemKind {
    Crate,
    Ore,
    Part,
}

impl ItemKind {
    pub const ALL: [ItemKind; 3] = [ItemKind::Crate, ItemKind::Ore, ItemKind::Part];

    pub fn label(&self) -> &'static str {
        match self {
            ItemKind::Crate => "Cargo Crate",
            ItemKind::Ore => "Asteroid Ore",
            ItemKind::Part => "Machinery Part",
        }
    }

    pub fn index(&self) -> usize {
        match self {
            ItemKind::Crate => 0,
            ItemKind::Ore => 1,
            ItemKind::Part => 2,
        }
    }
}

/// A physical item lying on the ground (or carried by a crew member).
#[derive(Component)]
pub struct Item {
    pub kind: ItemKind,
}

/// Player intent: "this item should end up in storage".
#[derive(Component)]
pub struct MarkedForHaul;

/// Claimed by a crew member's active haul job. Exactly one crew per item.
#[derive(Component)]
pub struct ReservedBy(pub Entity);

/// Set while a crew member is physically carrying the item.
#[derive(Component)]
pub struct CarriedBy(pub Entity);

/// Claim-time pathing failed; do not retry before this game-time seconds mark.
#[derive(Component)]
pub struct NoPathUntil(pub f64);

pub fn spawn_item(commands: &mut Commands, pos: crate::map::TilePos, kind: ItemKind) -> Entity {
    commands
        .spawn((
            crate::map::TilePos::new(pos.x, pos.y),
            Item { kind },
        ))
        .id()
}
