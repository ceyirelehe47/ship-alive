//! Player time-scale control: Pause / 1× / 2× / 4×.
//!
//! The scale itself lives in `GameSpeed`; the *pacing* of simulation time is
//! owned by `simtime` (`sim_pump_system` reads `GameSpeed` and steers the
//! fixed-update loop). Space toggles between Pause and the last non-paused
//! speed.

use crate::jobs::Action;
use crate::log::{EventLog, LogKind};
use crate::simtime::{speed_label, SPEED_SCALES};
use bevy::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq, Resource)]
pub struct GameSpeed {
    /// Index into [`crate::simtime::SPEED_SCALES`] (0 = paused).
    pub index: usize,
    /// Last explicitly chosen non-paused index (for Space resume).
    pub last_nonzero: usize,
}

impl Default for GameSpeed {
    fn default() -> Self {
        Self {
            index: 1,
            last_nonzero: 1,
        }
    }
}

impl GameSpeed {
    pub fn label(&self) -> &'static str {
        speed_label(self.index)
    }
}

pub struct TimeCtrlPlugin;

impl Plugin for TimeCtrlPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GameSpeed>();
        app.add_systems(
            Update,
            (speed_keys_system, speed_action_system)
                .chain()
                .in_set(crate::Set::Input),
        );
    }
}

fn speed_keys_system(keys: Res<ButtonInput<KeyCode>>, mut actions: EventWriter<Action>) {
    if keys.just_pressed(KeyCode::Space) {
        actions.write(Action::TogglePause);
    }
    if keys.just_pressed(KeyCode::Digit1) {
        actions.write(Action::SetSpeed { index: 1 });
    }
    if keys.just_pressed(KeyCode::Digit2) {
        actions.write(Action::SetSpeed { index: 2 });
    }
    if keys.just_pressed(KeyCode::Digit3) {
        actions.write(Action::SetSpeed { index: 3 });
    }
}

/// Apply speed actions: explicit picks remember the last non-paused speed;
/// TogglePause (Space) flips between Pause and that remembered speed.
pub fn speed_action_system(
    mut events: EventReader<Action>,
    mut speed: ResMut<GameSpeed>,
    mut log: ResMut<EventLog>,
    clock: Res<crate::simtime::SimClock>,
) {
    let now = clock.now();
    for action in events.read() {
        match *action {
            Action::SetSpeed { index } => {
                let index = index.min(SPEED_SCALES.len() - 1);
                if index == 0 {
                    // Pause request (speed button): pause, or resume when
                    // already paused.
                    if speed.index == 0 {
                        speed.index = speed.last_nonzero;
                    } else {
                        speed.index = 0;
                    }
                } else {
                    speed.index = index;
                    speed.last_nonzero = index;
                }
                log.push(now, LogKind::Info, format!("Speed: {}", speed.label()));
            }
            Action::TogglePause => {
                if speed.index == 0 {
                    speed.index = speed.last_nonzero;
                } else {
                    speed.index = 0;
                }
                log.push(now, LogKind::Info, format!("Speed: {}", speed.label()));
            }
            _ => {}
        }
    }
}
