use bevy::prelude::*;
use ship_alive::render;
use ship_alive::Set;
use ship_alive::{autotest, input, jobs, movement, setup, time_ctrl, ui};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Ship Alive — Slice 0".to_string(),
                resolution: (1440., 860.).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins((
            setup::SetupPlugin,
            autotest::AutotestPlugin,
            render::RenderPlugin,
            time_ctrl::TimeCtrlPlugin,
            input::InputPlugin,
            jobs::JobsPlugin,
            movement::MovementPlugin,
            ui::UiPlugin,
        ))
        // World resources must exist before the render plugin's startup systems.
        .add_systems(
            Startup,
            setup::setup_world
                .before(render::spawn_tile_visuals)
                .before(render::spawn_markers),
        )
        .configure_sets(Update, (Set::Input, Set::Jobs, Set::Move, Set::Sync).chain())
        .add_systems(Update, smoke_autoquit)
        .run();
}

/// Dev helper: `SLICE0_SMOKE=<frames> cargo run` exits automatically after N
/// frames, so the app can be smoke-tested from a script without a human
/// closing the window.
fn smoke_autoquit(mut frame: Local<u32>, mut exit: EventWriter<AppExit>) {
    if let Ok(limit) = std::env::var("SLICE0_SMOKE") {
        if let Ok(limit) = limit.parse::<u32>() {
            if *frame >= limit {
                exit.write(AppExit::Success);
            }
        }
    }
    *frame += 1;
}
