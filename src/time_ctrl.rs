//! Simulation speed control: Pause / 1x / 2x / 4x, driven by keys or UI buttons.

use crate::jobs::Action;
use crate::log::{EventLog, LogKind};
use bevy::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq, Resource)]
pub struct GameSpeed {
    /// Index into [`crate::SPEED_STEPS`] (0 = paused).
    pub index: usize,
}

impl Default for GameSpeed {
    fn default() -> Self {
        Self { index: 1 }
    }
}

impl GameSpeed {
    pub fn label(&self) -> &'static str {
        ["Paused", "1×", "2×", "4×"][self.index]
    }
}

pub struct TimeCtrlPlugin;

impl Plugin for TimeCtrlPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GameSpeed>();
        app.add_systems(
            Update,
            (speed_keys_system, speed_action_system, apply_speed_system)
                .chain()
                .in_set(crate::Set::Input),
        );
    }
}

fn speed_keys_system(keys: Res<ButtonInput<KeyCode>>, mut actions: EventWriter<Action>) {
    if keys.just_pressed(KeyCode::Space) {
        actions.write(Action::SetSpeed { index: 0 });
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

fn speed_action_system(
    mut events: EventReader<Action>,
    mut speed: ResMut<GameSpeed>,
    mut log: ResMut<EventLog>,
    time: Res<Time<Virtual>>,
) {
    let now = time.elapsed().as_secs_f64();
    for action in events.read() {
        if let Action::SetSpeed { index } = *action {
            let index = index.min(crate::SPEED_STEPS.len() - 1);
            if speed.index == index {
                continue;
            }
            // Space toggles between pause and the last non-paused speed.
            if index == 0 && speed.index == 0 {
                speed.index = 1;
            } else {
                speed.index = index;
            }
            log.push(now, LogKind::Info, format!("Speed: {}", speed.label()));
        }
    }
}

fn apply_speed_system(speed: Res<GameSpeed>, mut virtual_time: ResMut<Time<Virtual>>) {
    let multiplier = crate::SPEED_STEPS[speed.index];
    if multiplier == 0.0 {
        virtual_time.pause();
    } else {
        virtual_time.unpause();
        virtual_time.set_relative_speed(multiplier);
    }
}
