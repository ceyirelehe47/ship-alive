//! Storage: rack cells placed on floor tiles. Each cell holds a fixed number
//! of items of any kind. Rack tiles stay walkable — crew stand on them to store.

use crate::items::ItemKind;
use bevy::prelude::*;

/// Items a single rack cell can hold.
pub const RACK_CAPACITY: u32 = 4;

#[derive(Component)]
pub struct StorageCell {
    pub capacity: u32,
    /// Per-kind counts, indexed by `ItemKind::index`.
    pub counts: [u32; 3],
}

impl Default for StorageCell {
    fn default() -> Self {
        Self {
            capacity: RACK_CAPACITY,
            counts: [0, 0, 0],
        }
    }
}

impl StorageCell {
    pub fn stored(&self) -> u32 {
        self.counts.iter().sum()
    }

    pub fn free(&self) -> u32 {
        self.capacity.saturating_sub(self.stored())
    }

    pub fn has_space(&self) -> bool {
        self.free() > 0
    }

    /// Store one item of `kind`; returns false when the rack is already full.
    pub fn try_add(&mut self, kind: ItemKind) -> bool {
        if !self.has_space() {
            return false;
        }
        self.counts[kind.index()] += 1;
        true
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
}
