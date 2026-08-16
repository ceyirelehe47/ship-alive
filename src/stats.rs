//! Lightweight dev telemetry surfaced in the HUD and the autotest dumps.
//! Used to compare "bad layout vs. improved layout" runs during playtests.

use bevy::prelude::*;

#[derive(Resource, Default, Debug)]
pub struct Stats {
    pub built: u32,
    pub deconstructed: u32,
    /// Fabricator batches completed.
    pub produced: u32,
    pub hauls_done: u32,
    /// Total world-space tile-distance of haul paths accepted by crew
    /// (diagonal steps count as their true geometric length).
    pub haul_distance: f32,
}

impl Stats {
    pub fn summary(&self) -> String {
        format!(
            "built={} demo={} produced={} hauls={} haul_dist={:.0}",
            self.built, self.deconstructed, self.produced, self.hauls_done, self.haul_distance
        )
    }
}
