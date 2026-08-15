use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use ship_alive::render;
use ship_alive::Set;
use ship_alive::{autotest, input, jobs, movement, setup, time_ctrl, ui};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Ship Alive — Slice 1".to_string(),
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
        .configure_sets(
            Update,
            (Set::Input, Set::Jobs, Set::Move, Set::Sync).chain(),
        )
        .add_systems(Update, (smoke_autoquit, auto_screenshot))
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

/// Dev helper: `SLICE0_SHOT=<frame>[:<path>] cargo run` captures an in-engine
/// screenshot at the given frame (default `shot_auto.png`). Used for
/// scripted visual verification alongside the autotest scenarios.
fn auto_screenshot(mut frame: Local<u32>, mut commands: Commands) {
    if let Ok(spec) = std::env::var("SLICE0_SHOT") {
        let mut parts = spec.splitn(2, ':');
        let target: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(90);
        let path: String = parts.next().unwrap_or("shot_auto.png").to_string();
        if *frame == target {
            commands.spawn(Observer::new(save_to_disk(path.clone())));
            commands.spawn(Screenshot::primary_window());
        }
    }
    *frame += 1;
}
