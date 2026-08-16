//! Mouse/keyboard input: selection, box-select marking, camera control,
//! build tools (placement ghost + deconstruct marking), hotkeys and hover
//! detection.
//!
//! World picking maps the cursor to a tile and asks the queries who stands
//! there (priority: crew > item > rack > blueprint > building). Left click
//! selects; dragging on the map draws a box that marks every item inside for
//! hauling on release. While a build tool is active, left click places a
//! blueprint instead. Right-drag pans the camera. Pointer positions over UI
//! panels are ignored — any `Interaction` in hover state marks the pointer as
//! being over UI.

use crate::building::{Blueprint, Building, BuildingKind, MarkedForDeconstruct};
use crate::jobs::Action;
use crate::map::{ShipMap, TilePos};
use crate::power::CableGrid;
use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

#[derive(Clone, Copy, Debug)]
pub enum Selected {
    Crew(Entity),
    Item(Entity),
    Rack(Entity),
    Blueprint(Entity),
    Building(Entity),
}

#[derive(Resource, Default, Debug)]
pub struct Selection(pub Option<Selected>);

/// What the cursor currently hovers over (None over empty floor or UI).
#[derive(Resource, Default, Debug)]
pub struct Hovered(pub Option<Selected>);

/// Screen-space anchor of an in-progress left-drag box select.
#[derive(Resource, Default, Debug)]
pub struct BoxSelect {
    pub anchor: Option<Vec2>,
    pub current: Vec2,
    pub over_ui_at_press: bool,
}

/// The active build tool (None = normal selection mode).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Build(BuildingKind),
    Deconstruct,
}

#[derive(Resource, Default, Debug)]
pub struct BuildMode(pub Option<Tool>);

pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Selection>();
        app.init_resource::<Hovered>();
        app.init_resource::<BoxSelect>();
        app.init_resource::<BuildMode>();
        app.add_systems(
            Update,
            (
                tool_action_system,
                select_and_box_system,
                hover_system,
                action_keys_system,
                camera_control_system,
            )
                .in_set(crate::Set::Input),
        );
    }
}

/// Consume `SetTool` actions (fired by the build bar buttons and Esc).
fn tool_action_system(mut events: EventReader<Action>, mut mode: ResMut<BuildMode>) {
    for action in events.read() {
        if let Action::SetTool { tool } = *action {
            mode.0 = tool;
        }
    }
}

fn pointer_over_ui(ui: &Query<&Interaction, With<Node>>) -> bool {
    ui.iter()
        .any(|i| matches!(i, Interaction::Hovered | Interaction::Pressed))
}

/// Shared cursor→target picking (priority: crew > item > rack > blueprint >
/// building; multi-tile footprints are hit on any of their tiles).
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn pick_at_cursor(
    window: &Window,
    camera: &Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    map: &ShipMap,
    crews: &Query<(Entity, &TilePos), With<crate::crew::Crew>>,
    items: &Query<(Entity, &TilePos), With<crate::items::Item>>,
    racks: &Query<(Entity, &TilePos), With<crate::storage::StorageCell>>,
    blueprints: &Query<(Entity, &crate::building::Footprint), With<Blueprint>>,
    buildings: &Query<
        (
            Entity,
            &crate::building::Footprint,
            Option<&MarkedForDeconstruct>,
        ),
        (With<Building>, Without<Blueprint>),
    >,
) -> Option<(Selected, Vec2)> {
    let cursor = window.cursor_position()?;
    let Ok((cam, cam_gt)) = camera.single() else {
        return None;
    };
    let world = cam.viewport_to_world_2d(cam_gt, cursor).ok()?;
    let tile = map.tile_at_world(world)?;
    let target = if let Some((e, _)) = crews.iter().find(|(_, p)| **p == tile) {
        Some(Selected::Crew(e))
    } else if let Some((e, _)) = items.iter().find(|(_, p)| **p == tile) {
        Some(Selected::Item(e))
    } else if let Some((e, _)) = racks.iter().find(|(_, p)| **p == tile) {
        Some(Selected::Rack(e))
    } else if let Some((e, _)) = blueprints.iter().find(|(_, f)| f.contains(tile)) {
        Some(Selected::Blueprint(e))
    } else if let Some((e, _, _)) = buildings.iter().find(|(_, f, _)| f.contains(tile)) {
        Some(Selected::Building(e))
    } else {
        None
    };
    target.map(|t| (t, world))
}

/// Left click selects a target (or places a blueprint / marks deconstruction
/// while a tool is active); dragging further than a few pixels in select mode
/// turns into a box select that marks all items inside for hauling.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn select_and_box_system(
    buttons: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    ui: Query<&Interaction, With<Node>>,
    map: Res<ShipMap>,
    crews: Query<(Entity, &TilePos), With<crate::crew::Crew>>,
    items: Query<(Entity, &TilePos), With<crate::items::Item>>,
    racks: Query<(Entity, &TilePos), With<crate::storage::StorageCell>>,
    blueprints: Query<(Entity, &crate::building::Footprint), With<Blueprint>>,
    buildings: Query<
        (
            Entity,
            &crate::building::Footprint,
            Option<&MarkedForDeconstruct>,
        ),
        (With<Building>, Without<Blueprint>),
    >,
    mut selection: ResMut<Selection>,
    build_mode: Res<BuildMode>,
    // Nested tuple = one system param (the flat list would exceed 16).
    (cables, pipes, ducts): (
        Res<CableGrid>,
        Res<crate::coolant::PipeGrid>,
        Res<crate::ventilation::DuctGrid>,
    ),
    mut box_select: ResMut<BoxSelect>,
    mut actions: EventWriter<Action>,
    mut last_paint: Local<Option<TilePos>>,
) {
    let Ok(window) = windows.single() else {
        return;
    };

    // Drag-paint for underfloor runs: hold the left button and sweep tiles to
    // lay cable/pipe blueprints along the way (click also works: the press
    // frame paints).
    let drag_kind = match build_mode.0 {
        Some(Tool::Build(BuildingKind::PowerCable)) => Some(BuildingKind::PowerCable),
        Some(Tool::Build(BuildingKind::CoolantPipe)) => Some(BuildingKind::CoolantPipe),
        _ => None,
    };
    if let Some(kind) = drag_kind {
        if !pointer_over_ui(&ui) && buttons.pressed(MouseButton::Left) {
            if let Some(tile) = cursor_tile(window, &camera, &map) {
                if *last_paint != Some(tile) {
                    *last_paint = Some(tile);
                    actions.write(Action::PlaceBlueprint { kind, pos: tile });
                }
            }
        } else {
            *last_paint = None;
        }
    } else {
        *last_paint = None;
    }

    if buttons.just_pressed(MouseButton::Left) {
        box_select.anchor = window.cursor_position();
        box_select.over_ui_at_press = pointer_over_ui(&ui);
        box_select.current = box_select.anchor.unwrap_or_default();
    }

    if let Some(anchor) = box_select.anchor {
        if let Some(cursor) = window.cursor_position() {
            box_select.current = cursor;
        }
        if buttons.just_released(MouseButton::Left) {
            let dist = box_select.current.distance(anchor);
            if !box_select.over_ui_at_press && dist <= 10.0 {
                // Plain click: tool action, or select.
                if let Some(tool) = build_mode.0 {
                    match tool {
                        Tool::Build(BuildingKind::PowerCable | BuildingKind::CoolantPipe) => {
                            // Handled by drag-paint on the press frame.
                        }
                        Tool::Build(kind) => {
                            if let Some(tile) = cursor_tile(window, &camera, &map) {
                                actions.write(Action::PlaceBlueprint { kind, pos: tile });
                            }
                        }
                        Tool::Deconstruct => {
                            if let Some((Selected::Building(e) | Selected::Rack(e), _)) =
                                pick_at_cursor(
                                    window,
                                    &camera,
                                    &map,
                                    &crews,
                                    &items,
                                    &racks,
                                    &blueprints,
                                    &buildings,
                                )
                            {
                                let marked =
                                    buildings.get(e).ok().is_some_and(|(_, _, m)| m.is_some());
                                if marked {
                                    actions.write(Action::UnmarkDeconstruct { building: e });
                                } else {
                                    actions.write(Action::MarkDeconstruct { building: e });
                                }
                            } else if let Some(tile) = cursor_tile(window, &camera, &map) {
                                // No floor-side building: mark the underfloor
                                // cable instead (visible in the power overlay).
                                if cables.has(tile) {
                                    actions.write(Action::MarkCableDeconstruct { pos: tile });
                                } else if pipes.has(tile) {
                                    actions.write(Action::MarkPipeDeconstruct { pos: tile });
                                } else if ducts.has(tile) {
                                    actions.write(Action::MarkDuctDeconstruct { pos: tile });
                                }
                            }
                        }
                    }
                } else if let Some((target, _)) = pick_at_cursor(
                    window,
                    &camera,
                    &map,
                    &crews,
                    &items,
                    &racks,
                    &blueprints,
                    &buildings,
                ) {
                    selection.0 = Some(target);
                } else {
                    selection.0 = None;
                }
            } else if !box_select.over_ui_at_press && dist > 10.0 && build_mode.0.is_none() {
                // Box select: mark items between the two world-space corners.
                let Ok((cam, cam_gt)) = camera.single() else {
                    box_select.anchor = None;
                    return;
                };
                if let (Ok(from), Ok(to)) = (
                    cam.viewport_to_world_2d(cam_gt, anchor),
                    cam.viewport_to_world_2d(cam_gt, box_select.current),
                ) {
                    actions.write(Action::MarkArea { from, to });
                }
            }
            box_select.anchor = None;
        }
    }
}

fn cursor_tile(
    window: &Window,
    camera: &Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    map: &ShipMap,
) -> Option<TilePos> {
    let cursor = window.cursor_position()?;
    let Ok((cam, cam_gt)) = camera.single() else {
        return None;
    };
    let world = cam.viewport_to_world_2d(cam_gt, cursor).ok()?;
    map.tile_at_world(world)
}

/// Track what the cursor hovers over (drives the hover ring and tooltip).
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn hover_system(
    buttons: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    ui: Query<&Interaction, With<Node>>,
    map: Res<ShipMap>,
    crews: Query<(Entity, &TilePos), With<crate::crew::Crew>>,
    items: Query<(Entity, &TilePos), With<crate::items::Item>>,
    racks: Query<(Entity, &TilePos), With<crate::storage::StorageCell>>,
    blueprints: Query<(Entity, &crate::building::Footprint), With<Blueprint>>,
    buildings: Query<
        (
            Entity,
            &crate::building::Footprint,
            Option<&MarkedForDeconstruct>,
        ),
        (With<Building>, Without<Blueprint>),
    >,
    mut hovered: ResMut<Hovered>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    if window.cursor_position().is_none()
        || buttons.pressed(MouseButton::Left)
        || pointer_over_ui(&ui)
    {
        hovered.0 = None;
        return;
    }
    hovered.0 = pick_at_cursor(
        window,
        &camera,
        &map,
        &crews,
        &items,
        &racks,
        &blueprints,
        &buildings,
    )
    .map(|(t, _)| t);
}

fn action_keys_system(
    keys: Res<ButtonInput<KeyCode>>,
    debug: Option<Res<crate::ui::DebugBarVisible>>,
    worktab: Option<Res<crate::worktab::WorkTabVisible>>,
    mut selection: ResMut<Selection>,
    build_mode: Res<BuildMode>,
    mut actions: EventWriter<Action>,
) {
    if keys.just_pressed(KeyCode::KeyH) {
        actions.write(Action::MarkAll);
    }
    if keys.just_pressed(KeyCode::KeyP) {
        actions.write(Action::CycleOverlay);
    }
    if keys.just_pressed(KeyCode::Tab) {
        actions.write(Action::ToggleWorkTab);
    }
    if keys.just_pressed(KeyCode::KeyC) {
        actions.write(Action::CancelAll);
    }
    if keys.just_pressed(KeyCode::KeyT) {
        if let Some(Selected::Item(item)) = selection.0 {
            actions.write(Action::ToggleMark { item });
        }
    }
    if keys.just_pressed(KeyCode::KeyB) {
        // Cycle build tools: none → wall → door → rack → fabricator → cable
        // → reactor → pipe → pump → heat exchanger → radiator → reservoir →
        // deconstruct → none. (The full set also lives in the BUILD menu.)
        let next = match build_mode.0 {
            None => Some(Tool::Build(BuildingKind::Wall)),
            Some(Tool::Build(BuildingKind::Wall)) => Some(Tool::Build(BuildingKind::Door)),
            Some(Tool::Build(BuildingKind::Door)) => Some(Tool::Build(BuildingKind::Rack)),
            Some(Tool::Build(BuildingKind::Rack)) => Some(Tool::Build(BuildingKind::Fabricator)),
            Some(Tool::Build(BuildingKind::Fabricator)) => {
                Some(Tool::Build(BuildingKind::PowerCable))
            }
            Some(Tool::Build(BuildingKind::PowerCable)) => Some(Tool::Build(BuildingKind::Reactor)),
            Some(Tool::Build(BuildingKind::Reactor)) => {
                Some(Tool::Build(BuildingKind::CoolantPipe))
            }
            Some(Tool::Build(BuildingKind::CoolantPipe)) => Some(Tool::Build(BuildingKind::Pump)),
            Some(Tool::Build(BuildingKind::Pump)) => Some(Tool::Build(BuildingKind::HeatExchanger)),
            Some(Tool::Build(BuildingKind::HeatExchanger)) => {
                Some(Tool::Build(BuildingKind::Radiator))
            }
            Some(Tool::Build(BuildingKind::Radiator)) => Some(Tool::Build(BuildingKind::Reservoir)),
            Some(Tool::Build(BuildingKind::Reservoir)) => Some(Tool::Build(BuildingKind::GasDuct)),
            Some(Tool::Build(BuildingKind::GasDuct)) => Some(Tool::Build(BuildingKind::Vent)),
            Some(Tool::Build(BuildingKind::Vent)) => Some(Tool::Build(BuildingKind::Blower)),
            Some(Tool::Build(BuildingKind::Blower)) => Some(Tool::Build(BuildingKind::GasTank)),
            Some(Tool::Build(BuildingKind::GasTank)) => Some(Tool::Deconstruct),
            Some(Tool::Deconstruct) => None,
        };
        actions.write(Action::SetTool { tool: next });
    }
    if keys.just_pressed(KeyCode::KeyX) && debug.is_some_and(|d| d.0) {
        if let Some(Selected::Item(item)) = selection.0 {
            actions.write(Action::DeleteItem { item });
        }
    }
    if keys.just_pressed(KeyCode::Escape) {
        if worktab.is_some_and(|w| w.0) {
            // The WORK tab closes first; a second Esc clears tools/selection.
            actions.write(Action::ToggleWorkTab);
        } else if build_mode.0.is_some() {
            actions.write(Action::SetTool { tool: None });
        } else {
            selection.0 = None;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn camera_control_system(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut wheel_events: EventReader<MouseWheel>,
    // Real (not virtual) time: the camera is a presentation concern and must
    // pan at a constant real-world speed regardless of game speed — the
    // virtual clock runs at BASE_SIM_RATE × scale, and it also freezes on
    // pause.
    time: Res<Time<Real>>,
    map: Res<ShipMap>,
    mut camera: Query<(&mut Transform, &mut Projection), With<Camera2d>>,
    mut last_cursor: Local<Option<Vec2>>,
    mut zoom_target: Local<f32>,
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

    // Right-drag pan: camera follows the pointer one-to-one.
    if let Ok(window) = windows.single() {
        let cursor = window.cursor_position();
        if buttons.pressed(MouseButton::Right) {
            if let (Some(cur), Some(prev)) = (cursor, *last_cursor) {
                transform.translation.x -= (cur.x - prev.x) * scale;
                transform.translation.y += (cur.y - prev.y) * scale;
            }
            *last_cursor = cursor;
        } else {
            *last_cursor = cursor;
        }
    }

    // Wheel zoom, smoothed toward the target scale.
    if *zoom_target == 0.0 {
        *zoom_target = scale; // first frame initialization
    }
    let mut zoom_delta = 0.0;
    for scroll in wheel_events.read() {
        zoom_delta += scroll.y;
    }
    if zoom_delta != 0.0 {
        *zoom_target = (*zoom_target * (1.0 - 0.15 * zoom_delta)).clamp(0.6, 3.0);
    }
    if let Projection::Orthographic(ref mut o) = *projection {
        let t = (time.delta().as_secs_f32() * 12.0).min(1.0);
        o.scale += (*zoom_target - o.scale) * t;
    }

    // Keep the camera loosely within the ship bounds.
    let half_w = map.width as f32 * crate::TILE;
    let half_h = map.height as f32 * crate::TILE;
    let margin = 220.0 * scale;
    transform.translation.x = transform.translation.x.clamp(-margin, half_w + margin);
    transform.translation.y = transform.translation.y.clamp(-half_h - margin, margin);
}
