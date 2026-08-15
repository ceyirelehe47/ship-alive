//! Mouse/keyboard input: selection, camera control and hotkeys.
//!
//! World picking is done by mapping the cursor to a tile and asking the
//! queries who stands there (priority: crew > item > rack). Clicks that land
//! on UI panels are ignored — any `Interaction` in hover state marks the
//! pointer as being over UI.

use crate::jobs::Action;
use crate::map::{ShipMap, TilePos};
use bevy::prelude::*;
use bevy::input::mouse::MouseWheel;
use bevy::window::PrimaryWindow;

#[derive(Clone, Copy, Debug)]
pub enum Selected {
    Crew(Entity),
    Item(Entity),
    Rack(Entity),
}

#[derive(Resource, Default, Debug)]
pub struct Selection(pub Option<Selected>);

pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Selection>();
        app.add_systems(
            Update,
            (click_select_system, action_keys_system, camera_control_system)
                .in_set(crate::Set::Input),
        );
    }
}

fn pointer_over_ui(ui: &Query<&Interaction, With<Node>>) -> bool {
    ui.iter().any(|i| matches!(i, Interaction::Hovered | Interaction::Pressed))
}

#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn click_select_system(
    buttons: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    ui: Query<&Interaction, With<Node>>,
    map: Res<ShipMap>,
    crews: Query<(Entity, &TilePos), With<crate::crew::Crew>>,
    items: Query<(Entity, &TilePos), With<crate::items::Item>>,
    racks: Query<(Entity, &TilePos), With<crate::storage::StorageCell>>,
    mut selection: ResMut<Selection>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    if !buttons.just_pressed(MouseButton::Left) || pointer_over_ui(&ui) {
        return;
    }
    let Ok((cam, cam_gt)) = camera.single() else {
        return;
    };
    let Some(world) = cam.viewport_to_world_2d(cam_gt, cursor).ok() else {
        return;
    };
    let Some(tile) = map.tile_at_world(world) else {
        selection.0 = None;
        return;
    };

    if let Some((e, _)) = crews.iter().find(|(_, p)| **p == tile) {
        selection.0 = Some(Selected::Crew(e));
    } else if let Some((e, _)) = items.iter().find(|(_, p)| **p == tile) {
        selection.0 = Some(Selected::Item(e));
    } else if let Some((e, _)) = racks.iter().find(|(_, p)| **p == tile) {
        selection.0 = Some(Selected::Rack(e));
    } else {
        selection.0 = None;
    }
}

fn action_keys_system(
    keys: Res<ButtonInput<KeyCode>>,
    mut selection: ResMut<Selection>,
    mut actions: EventWriter<Action>,
) {
    if keys.just_pressed(KeyCode::KeyH) {
        actions.write(Action::MarkAll);
    }
    if keys.just_pressed(KeyCode::KeyC) {
        actions.write(Action::CancelAll);
    }
    if keys.just_pressed(KeyCode::KeyT) {
        if let Some(Selected::Item(item)) = selection.0 {
            actions.write(Action::ToggleMark { item });
        }
    }
    if keys.just_pressed(KeyCode::KeyX) {
        if let Some(Selected::Item(item)) = selection.0 {
            actions.write(Action::DeleteItem { item });
        }
    }
    if keys.just_pressed(KeyCode::Escape) {
        selection.0 = None;
    }
}

fn camera_control_system(
    keys: Res<ButtonInput<KeyCode>>,
    mut wheel_events: EventReader<MouseWheel>,
    time: Res<Time>,
    map: Res<ShipMap>,
    mut camera: Query<(&mut Transform, &mut Projection), With<Camera2d>>,
) {
    let Ok((mut transform, mut projection)) = camera.single_mut() else {
        return;
    };
    let scale = match &*projection {
        Projection::Orthographic(o) => o.scale,
        _ => 1.0,
    };

    // Keyboard pan.
    let mut dir = Vec2::ZERO;
    if keys.pressed(KeyCode::KeyW) || keys.pressed(KeyCode::ArrowUp) {
        dir.y += 1.0;
    }
    if keys.pressed(KeyCode::KeyS) || keys.pressed(KeyCode::ArrowDown) {
        dir.y -= 1.0;
    }
    if keys.pressed(KeyCode::KeyA) || keys.pressed(KeyCode::ArrowLeft) {
        dir.x -= 1.0;
    }
    if keys.pressed(KeyCode::KeyD) || keys.pressed(KeyCode::ArrowRight) {
        dir.x += 1.0;
    }

    if dir != Vec2::ZERO {
        let step = 420.0 * scale * time.delta().as_secs_f32();
        transform.translation.x += dir.x * step;
        transform.translation.y += dir.y * step;
    }

    // Wheel zoom (2.0 = half size, 0.5 = double size).
    let mut zoom_delta = 0.0;
    for scroll in wheel_events.read() {
        zoom_delta += scroll.y;
    }
    if zoom_delta != 0.0 {
        if let Projection::Orthographic(ref mut o) = *projection {
            o.scale = (o.scale * (1.0 - 0.1 * zoom_delta)).clamp(0.6, 3.0);
        }
    }

    // Keep the camera loosely within the ship bounds.
    let half_w = map.width as f32 * crate::TILE;
    let half_h = map.height as f32 * crate::TILE;
    let margin = 300.0 * scale;
    transform.translation.x = transform.translation.x.clamp(-margin, half_w + margin);
    transform.translation.y = transform.translation.y.clamp(-half_h - margin, margin);
}
