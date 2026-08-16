//! Unified Simulation Time — the single authoritative clock for all gameplay.
//!
//! Three separated concepts:
//! - **Real time** (`Time<Real>`): wall clock; only drives the scheduler,
//!   rendering and UI. Gameplay never reads it directly.
//! - **Simulation time** (`SimClock`): the ship's continuous timeline,
//!   `T+HHH:MM:SS`, hours roll past 24 without wrapping, no game days.
//!   Measured internally in whole microseconds (`i64`) so 1000+ hour runs
//!   keep exact second-level precision.
//! - **Player time scale** (`GameSpeed`): Pause / 1× / 2× / 4× — how much
//!   simulation time one real second produces. It never changes world rules.
//!
//! Scheduling model: continuous gameplay systems (jobs, power, movement) run
//! in Bevy's `FixedUpdate` schedule with a fixed step of `SIM_STEP` sim
//! seconds. `sim_pump_system` (PreUpdate) converts real delta into requested
//! sim advance (`real_dt × BASE_SIM_RATE × scale`) and steers
//! `Time<Virtual>`; the engine's fixed loop then executes 0..N steps per
//! frame, flushing commands between steps, so high speeds NEVER re-run the
//! whole frame's logic manually and N steps behave exactly like N frames at
//! 1×. `Time<Fixed>` follows `Time<Virtual>` (which runs at
//! `BASE_SIM_RATE × scale`), so the fixed timestep is `SIM_STEP` of virtual
//! time — the loop executes exactly `BASE_SIM_RATE × scale` steps per real
//! second, matching what the backlog gate dispenses.
//! `Time<Virtual>::max_delta` (250 ms) caps per-frame catch-up so a
//! hitch cannot spiral; `SimClock` only advances inside FixedUpdate, hence
//! the clock can never run ahead of actually-processed world state. Leftover
//! unprocessed time is reported as `backlog`.

use crate::time_ctrl::GameSpeed;
use bevy::prelude::*;
use std::time::Duration;

/// Simulation seconds advanced per real second at player scale 1×.
///
/// 60 was chosen so gameplay durations become readable ship time (a 6 real-s
/// fabrication cycle is exactly "6 ship minutes") while the 1× feel is
/// preserved by migrating every gameplay constant into sim seconds (×60).
/// Central, temporary, and adjustable — see REPORT_TIME.md.
pub const BASE_SIM_RATE: f64 = 60.0;

/// Fixed simulation step (in sim seconds) for continuous systems.
/// One ship minute per step: 60 steps per real second at 1×, 240 at 4×.
pub const SIM_STEP: f64 = 1.0;

/// Player time scales, single source of truth (index 0 = pause).
pub const SPEED_SCALES: [f64; 4] = [0.0, 1.0, 2.0, 4.0];

/// The authoritative simulation clock. Internal time is integer microseconds
/// so long campaigns (1000 h = 3.6e12 µs) never lose second-level precision.
#[derive(Resource)]
pub struct SimClock {
    elapsed_us: i64,
    /// dt (sim seconds) visible to gameplay systems during the current run.
    step_dt: f64,
    /// Sim time offered by the pump but not yet executed (catch-up backlog).
    backlog_us: i64,
    /// Steps executed during the current frame (moved to `steps_last_frame`
    /// by the PostUpdate telemetry bookkeeping).
    pending_steps: u64,
    /// Telemetry: steps executed in the last frame.
    pub steps_last_frame: u64,
    /// Telemetry: peak steps in any single frame since startup.
    pub peak_steps: u64,
}

impl Default for SimClock {
    fn default() -> Self {
        Self {
            elapsed_us: 0,
            step_dt: 0.0,
            backlog_us: 0,
            pending_steps: 0,
            steps_last_frame: 0,
            peak_steps: 0,
        }
    }
}

impl SimClock {
    /// Current simulation time in seconds (exact for whole microseconds).
    pub fn now(&self) -> f64 {
        self.elapsed_us as f64 / 1e6
    }

    /// dt of the current tick in sim seconds (fixed `SIM_STEP` in the app;
    /// test harnesses may drive manual slices).
    pub fn dt(&self) -> f64 {
        self.step_dt
    }

    /// Unprocessed offered sim time in seconds (catch-up backlog).
    pub fn backlog_secs(&self) -> f64 {
        self.backlog_us as f64 / 1e6
    }

    /// Convert a real-time duration (at 1×) into sim seconds — the migration
    /// helper every gameplay constant is expressed through.
    pub fn real_secs_to_sim(real: f64) -> f64 {
        real * BASE_SIM_RATE
    }

    // ---- app-side driving -------------------------------------------------

    /// Called once per frame (PreUpdate): convert real delta + player scale
    /// into requested sim advance and steer the virtual clock that paces the
    /// FixedUpdate loop.
    pub fn offer_real_delta(&mut self, real_dt: f64, scale: f64) {
        let advance_sim = real_dt * BASE_SIM_RATE * scale;
        self.backlog_us = (self.backlog_us as f64 + advance_sim * 1e6) as i64;
    }

    /// Executed by `sim_tick_system` inside FixedUpdate: advance exactly one
    /// fixed step (pulling from the backlog). Returns false when the frame's
    /// budget is exhausted — the rest stays as backlog for later frames.
    pub fn begin_fixed_step(&mut self) -> bool {
        if self.backlog_us < (SIM_STEP * 1e6) as i64 {
            return false;
        }
        self.backlog_us -= (SIM_STEP * 1e6) as i64;
        self.elapsed_us += (SIM_STEP * 1e6) as i64;
        self.step_dt = SIM_STEP;
        self.pending_steps += 1;
        true
    }

    /// Called once per frame after the fixed loop: publish the frame's step
    /// count telemetry.
    pub fn frame_bookkeeping(&mut self) {
        self.steps_last_frame = self.pending_steps;
        self.peak_steps = self.peak_steps.max(self.pending_steps);
        self.pending_steps = 0;
    }

    // ---- test/manual driving ----------------------------------------------

    /// Advance by an explicit sim-time slice (headless tests). The dt seen
    /// by systems equals `secs`, mirroring the old variable-step harnesses.
    pub fn advance_sim(&mut self, secs: f64) {
        self.elapsed_us = (self.elapsed_us as f64 + secs * 1e6).round() as i64;
        self.step_dt = secs;
    }
}

/// `T+HHH:MM:SS` — cumulative hours (≥ 24 and ≥ 1000 keep counting), no days.
pub fn format_sim_stamp(sim_secs: f64) -> String {
    let total = sim_secs.trunc();
    let hours = (total / 3600.0) as u64;
    let minutes = ((total / 60.0) % 60.0) as u64;
    let secs = (total % 60.0) as u64;
    format!("T+{hours:03}:{minutes:02}:{secs:02}")
}

/// Compact duration in ship terms: minutes below an hour, else H:MM.
pub fn format_sim_duration(sim_secs: f64) -> String {
    if sim_secs < 60.0 {
        format!("{sim_secs:.0}s")
    } else if sim_secs < 3600.0 {
        format!("{:.0}m", sim_secs / 60.0)
    } else {
        format!(
            "{}:{:02}h",
            (sim_secs / 3600.0) as u64,
            ((sim_secs % 3600.0) / 60.0) as u64
        )
    }
}

/// Player-facing speed labels derived from the single `SPEED_SCALES` table.
pub fn speed_label(index: usize) -> &'static str {
    match SPEED_SCALES.get(index) {
        Some(0.0) => "Paused",
        Some(1.0) => "1x",
        Some(2.0) => "2x",
        Some(4.0) => "4x",
        _ => "Paused",
    }
}

pub struct SimTimePlugin;

impl Plugin for SimTimePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SimClock>();
        // Steer the virtual clock BEFORE the fixed-update loop runs this
        // frame; TimeSystem (which produces the real delta) runs first in
        // PreUpdate.
        app.add_systems(PreUpdate, sim_pump_system);
        // Inside FixedUpdate: advance the authoritative clock by one fixed
        // step before any gameplay system reads it.
        app.add_systems(FixedUpdate, sim_tick_system.before(crate::Set::Jobs));
        // Telemetry bookkeeping after the loop, still frame-side.
        app.add_systems(PostUpdate, sim_telemetry_system);
    }
}

/// Frame-side: convert real time × base rate × player scale into requested
/// sim advance and pace `Time<Virtual>` so the engine's fixed loop executes
/// exactly the corresponding number of `SIM_STEP`s. `max_delta` (250 ms)
/// bounds catch-up so one long frame cannot spiral.
fn sim_pump_system(
    real: Res<Time<Real>>,
    mut virtual_time: ResMut<Time<Virtual>>,
    mut fixed_time: ResMut<Time<Fixed>>,
    speed: Res<GameSpeed>,
    mut clock: ResMut<SimClock>,
) {
    let scale = SPEED_SCALES.get(speed.index).copied().unwrap_or(0.0);
    // Offer the requested sim advance (clamped by Bevy's max_delta on the
    // real delta itself).
    let real_dt = real.delta().as_secs_f64();
    clock.offer_real_delta(real_dt, scale);
    // Pace the fixed loop: virtual relative speed maps real seconds to sim
    // seconds; pausing stops step accumulation entirely.
    let effective = BASE_SIM_RATE as f32 * scale as f32;
    if effective <= 0.0 {
        virtual_time.pause();
    } else {
        virtual_time.unpause();
        virtual_time.set_relative_speed(effective);
    }
    virtual_time.set_max_delta(Duration::from_millis(250));
    // Fixed step in VIRTUAL seconds (Time<Fixed> follows Time<Virtual>, which
    // already runs at BASE_SIM_RATE × scale): one step = one SIM_STEP. The
    // loop then executes 60 × scale steps per real second, exactly matching
    // the backlog the SimClock gate dispenses. (A timestep of
    // SIM_STEP / BASE_SIM_RATE was a real-seconds unit slip that made the
    // fixed loop run 60× too often — every extra run burned CPU while the
    // backlog gate kept the dt at 0, which at 4× collapsed the frame rate.)
    fixed_time.set_timestep(Duration::from_secs_f64(SIM_STEP));
}

/// Runs inside FixedUpdate, before gameplay: pull one fixed step from the
/// backlog. The loop pacing (timestep = SIM_STEP virtual seconds) offers
/// exactly as many runs as the backlog holds, so this normally succeeds on
/// every run; the gate stays as a defensive bound, marking `dt() == 0.0`
/// for any stray run so gameplay systems cheaply no-op.
fn sim_tick_system(mut clock: ResMut<SimClock>, mut commands: Commands) {
    if !clock.begin_fixed_step() {
        let _ = &mut commands;
        clock.step_dt = 0.0;
    }
}

/// Telemetry after the loop: how many steps actually ran this frame.
fn sim_telemetry_system(mut clock: ResMut<SimClock>) {
    clock.frame_bookkeeping();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_formats_cumulative_hours() {
        assert_eq!(format_sim_stamp(0.0), "T+000:00:00");
        assert_eq!(format_sim_stamp(3909.0), "T+001:05:09");
        assert_eq!(format_sim_stamp(86399.0), "T+023:59:59");
        assert_eq!(format_sim_stamp(86400.0), "T+024:00:00"); // no day wrap
        assert_eq!(format_sim_stamp(86401.0), "T+024:00:01");
        assert_eq!(format_sim_stamp(460_992.0), "T+128:03:12");
        assert_eq!(format_sim_stamp(3_600_000.0), "T+1000:00:00");
    }

    #[test]
    fn duration_formats_ship_terms() {
        assert_eq!(format_sim_duration(42.0), "42s");
        assert_eq!(format_sim_duration(360.0), "6m");
        assert_eq!(format_sim_duration(5430.0), "1:30h");
    }

    #[test]
    fn pump_and_steps_match_requested_advance() {
        let mut c = SimClock::default();
        // 1× for one real second = 60 sim s = 60 steps.
        c.offer_real_delta(1.0, 1.0);
        let mut steps = 0;
        while c.begin_fixed_step() {
            steps += 1;
        }
        assert_eq!(steps, 60);
        assert_eq!(c.now(), 60.0);
        assert!(c.backlog_secs() < SIM_STEP);

        // 4× for one real second = 240 sim s.
        let mut c4 = SimClock::default();
        c4.offer_real_delta(1.0, 4.0);
        let mut s4 = 0;
        while c4.begin_fixed_step() {
            s4 += 1;
        }
        assert_eq!(s4, 240);
        assert_eq!(c4.now(), 240.0);
    }

    #[test]
    fn paused_offers_nothing() {
        let mut c = SimClock::default();
        c.offer_real_delta(10.0, 0.0);
        assert!(!c.begin_fixed_step());
        assert_eq!(c.now(), 0.0);
    }

    #[test]
    fn fractional_real_frames_accumulate() {
        let mut c = SimClock::default();
        // 60 fps at 1×: 60 frames × 1/60 s real = 60 sim s total.
        for _ in 0..60 {
            c.offer_real_delta(1.0 / 60.0, 1.0);
            while c.begin_fixed_step() {}
        }
        assert_eq!(c.now(), 60.0);
    }

    #[test]
    fn irregular_frames_reach_same_time() {
        let chunks = [0.016, 0.016, 0.033, 0.008, 0.050, 0.024, 0.016];
        let total: f64 = chunks.iter().sum();
        let mut a = SimClock::default();
        for dt in chunks {
            a.offer_real_delta(dt, 1.0);
            while a.begin_fixed_step() {}
        }
        let mut b = SimClock::default();
        b.offer_real_delta(total, 1.0);
        while b.begin_fixed_step() {}
        assert_eq!(a.now(), b.now(), "irregular vs one chunk must agree");
    }

    #[test]
    fn long_run_precision_stays_exact() {
        let mut c = SimClock::default();
        // 1000 hours in steps of 1 sim s.
        let target = 1000.0 * 3600.0;
        c.offer_real_delta(target / BASE_SIM_RATE / 1.0, 1.0);
        while c.begin_fixed_step() {}
        assert_eq!(c.now(), target);
        assert_eq!(format_sim_stamp(c.now()), "T+1000:00:00");
        // Even at 10,000 hours the second count is exact (integer µs core).
        let mut c2 = SimClock::default();
        c2.offer_real_delta(10_000.0 * 3600.0 / BASE_SIM_RATE, 1.0);
        while c2.begin_fixed_step() {}
        assert_eq!(c2.now() % 1.0, 0.0);
        assert_eq!(format_sim_stamp(c2.now()), "T+10000:00:00");
    }
}
