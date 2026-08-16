use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use ship_alive::render;
use ship_alive::Set;
use ship_alive::{autotest, input, jobs, movement, setup, time_ctrl, ui};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Ship Alive".to_string(),
                resolution: (1440., 860.).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins((
            setup::SetupPlugin,
            autotest::AutotestPlugin,
            ship_alive::simtime::SimTimePlugin,
            ship_alive::power::PowerPlugin,
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
        // Gameplay advances in fixed sim steps (SimClock-driven); input and
        // presentation stay frame-based. FixedUpdate runs before Update
        // within a frame, so Sync always renders the latest world state.
        .configure_sets(FixedUpdate, (Set::Jobs, Set::Move).chain())
        .configure_sets(Update, (Set::Input, Set::Sync).chain())
        .add_systems(Update, (smoke_autoquit, auto_screenshot, ui_layout_debug))
        .run();
}

/// Dev helper: `SLICE0_UI_DEBUG=1` prints the computed taffy layout of every
/// Interaction-bearing HUD node once at frame 250, with the first text found
/// under it (2 levels deep) so panels can be identified in the log.
fn ui_layout_debug(
    mut frame: Local<u32>,
    nodes: Query<(Entity, &ComputedNode, &GlobalTransform, &Children), With<Interaction>>,
    texts: Query<&Text>,
) {
    if std::env::var("SLICE0_UI_DEBUG").is_err() {
        return;
    }
    if *frame != 250 {
        *frame += 1;
        return;
    }
    let mut rows: Vec<(f32, f32, f32, f32, String)> = Vec::new();
    for (e, node, gt, children) in nodes.iter() {
        let s = node.size;
        let mut tag = String::new();
        for c in children.iter() {
            if let Ok(t) = texts.get(c) {
                tag = t.0.chars().take(18).collect();
                break;
            }
        }
        if tag.is_empty() {
            tag = format!("e{:?}", e.index());
        }
        rows.push((gt.translation().y, gt.translation().x, s.x, s.y, tag));
    }
    rows.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    });
    println!("---- UI LAYOUT (y, x, w, h, first-text) ----");
    for (y, x, w, h, tag) in rows {
        println!("UIDBG y={y:<7.0} x={x:<7.0} {w:<6.0}x{h:<5.0} {tag}");
    }
}

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
