//! Storage: rack cells placed on floor tiles. Each cell holds a fixed number
//! of items. Rack tiles stay walkable — crew stand on them to store.
//!
//! Slice 1 adds per-kind filters so the player can dedicate racks ("raw
//! material racks" near a fabricator input, "parts racks" near its output),
//! which is the main lever for layout-driven logistics optimization.

use crate::items::ItemKind;
use bevy::prelude::*;

/// Items a single rack cell can hold.
pub const RACK_CAPACITY: u32 = 4;

#[derive(Component)]
pub struct StorageCell {
    pub capacity: u32,
    /// Per-kind counts, indexed by `ItemKind::index`.
    pub counts: [u32; 3],
    /// Per-kind acceptance, indexed by `ItemKind::index`. Default: all allowed.
    pub allowed: [bool; 3],
}

impl Default for StorageCell {
    fn default() -> Self {
        Self {
            capacity: RACK_CAPACITY,
            counts: [0, 0, 0],
            allowed: [true, true, true],
        }
    }
}

impl StorageCell {
    pub fn with_stock(kind: ItemKind, n: u32) -> Self {
        let mut s = Self::default();
        s.counts[kind.index()] = n.min(s.capacity);
        s
    }

    pub fn stored(&self) -> u32 {
        self.counts.iter().sum()
    }

    pub fn free(&self) -> u32 {
        self.capacity.saturating_sub(self.stored())
    }

    pub fn has_space(&self) -> bool {
        self.free() > 0
    }

    pub fn accepts(&self, kind: ItemKind) -> bool {
        self.allowed[kind.index()]
    }

    /// Can this rack take one item of `kind` right now?
    pub fn can_take(&self, kind: ItemKind) -> bool {
        self.accepts(kind) && self.has_space()
    }

    /// Does the rack hold at least one item of `kind` (pullable for a job)?
    pub fn has_kind(&self, kind: ItemKind) -> bool {
        self.counts[kind.index()] > 0
    }

    /// Store one item of `kind`; returns false when the rack cannot take it.
    pub fn try_add(&mut self, kind: ItemKind) -> bool {
        if !self.can_take(kind) {
            return false;
        }
        self.counts[kind.index()] += 1;
        true
    }

    /// Remove one item of `kind` (a hauler pulling stock for a job).
    pub fn take(&mut self, kind: ItemKind) -> bool {
        if !self.has_kind(kind) {
            return false;
        }
        self.counts[kind.index()] -= 1;
        true
    }

    /// Short filter summary for labels, e.g. "Ore" or "Ore+Part".
    pub fn filter_label(&self) -> String {
        let names: Vec<&str> = ItemKind::ALL
            .iter()
            .filter(|k| self.allowed[k.index()])
            .map(|k| match k {
                ItemKind::Crate => "Crate",
                ItemKind::Ore => "Ore",
                ItemKind::Part => "Part",
            })
            .collect();
        if names.len() == ItemKind::ALL.len() {
            "any".into()
        } else {
            names.join("+")
        }
    }

    pub fn label(&self) -> String {
        format!("{}/{}", self.stored(), self.capacity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_limits() {
        let mut s = StorageCell::default();
        for _ in 0..4 {
            assert!(s.try_add(ItemKind::Ore));
        }
        assert!(!s.has_space());
        assert!(!s.try_add(ItemKind::Crate));
        assert_eq!(s.counts[ItemKind::Ore.index()], 4);
        assert_eq!(s.label(), "4/4");
    }

    #[test]
    fn filter_rejects_wrong_kind() {
        let mut s = StorageCell {
            allowed: [false, true, false], // ore only
            ..StorageCell::default()
        };
        assert!(s.can_take(ItemKind::Ore));
        assert!(!s.can_take(ItemKind::Part));
        assert!(!s.try_add(ItemKind::Part));
        assert!(s.try_add(ItemKind::Ore));
        assert_eq!(s.filter_label(), "Ore");
    }

    #[test]
    fn pull_from_stock() {
        let mut s = StorageCell::with_stock(ItemKind::Part, 3);
        assert!(s.take(ItemKind::Part));
        assert_eq!(s.stored(), 2);
        assert!(!s.take(ItemKind::Ore));
        let mut empty = StorageCell::with_stock(ItemKind::Ore, 0);
        assert!(!empty.take(ItemKind::Ore));
    }
}
