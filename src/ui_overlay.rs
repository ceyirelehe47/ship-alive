//! Cursor-following tooltip and the box-select rectangle overlay.
//!
//! The tooltip shows a title + one status line for whatever the cursor
//! hovers (crew / item / rack); the box-select rect is drawn while the left
//! button is dragged on the map. Both are plain UI nodes updated per frame.

use crate::building::MarkedForDeconstruct;
use crate::building::{Blueprint, Building};
use crate::crew::{Crew, CrewTask, Movement};
use crate::input::{BoxSelect, Hovered, Selected};
use crate::items::{CarriedBy, Item, MarkedForHaul, NoPathUntil, ReservedBy};
use crate::loc::{self, strings, Lang};
use crate::map::TilePos;
use crate::storage::StorageCell;
use crate::ui::{item_status, task_label};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

const TOOLTIP_BG: Color = Color::srgba(0.04, 0.05, 0.08, 0.92);
const TOOLTIP_W: f32 = 270.0;

#[derive(Resource)]
pub struct Overlay {
    pub tooltip: Entity,
    pub tooltip_title: Entity,
    pub tooltip_detail: Entity,
    pub box_rect: Entity,
}

pub fn build_overlay(mut commands: Commands) {
    let mut tooltip_title = Entity::PLACEHOLDER;
    let mut tooltip_detail = Entity::PLACEHOLDER;
    let tooltip = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                width: Val::Px(TOOLTIP_W),
                padding: UiRect::all(Val::Px(6.0)),
                flex_direction: FlexDirection::Column,
                ..default()
            },
            BackgroundColor(TOOLTIP_BG),
            Visibility::Hidden,
            ZIndex(10),
        ))
        .with_children(|p| {
            tooltip_title = p
                .spawn((
                    Text::new(""),
                    TextFont {
                        font_size: 13.0,
                        ..default()
                    },
                    TextColor(Color::WHITE),
                ))
                .id();
            tooltip_detail = p
                .spawn((
                    Text::new(""),
                    TextFont {
                        font_size: 12.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.72, 0.76, 0.82)),
                ))
                .id();
        })
        .id();

    let box_rect = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BorderColor(Color::srgba(0.4, 0.9, 1.0, 0.9)),
            BackgroundColor(Color::srgba(0.4, 0.9, 1.0, 0.08)),
            Visibility::Hidden,
            ZIndex(9),
        ))
        .id();

    commands.insert_resource(Overlay {
        tooltip,
        tooltip_title,
        tooltip_detail,
        box_rect,
    });
}

/// Follow the cursor with a small info card for the hovered target.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
pub fn tooltip_system(
    windows: Query<&Window, With<PrimaryWindow>>,
    hovered: Res<Hovered>,
    lang: Res<Lang>,
    clock: Res<crate::simtime::SimClock>,
    overlay: Res<Overlay>,
    crews: Query<(Entity, &Crew, &CrewTask, &TilePos, &Movement)>,
    items: Query<
        (
            Entity,
            &TilePos,
            &Item,
            Option<&MarkedForHaul>,
            Option<&ReservedBy>,
            Option<&CarriedBy>,
            Option<&NoPathUntil>,
        ),
        With<Item>,
    >,
    racks: Query<(
        Entity,
        &TilePos,
        &StorageCell,
        Option<&Building>,
        Option<&MarkedForDeconstruct>,
    )>,
    blueprints: Query<(&TilePos, &Blueprint)>,
    buildings: Query<(&TilePos, &Building, Option<&crate::airtight::Door>), Without<Blueprint>>,
    mut node_q: Query<&mut Node, With<ZIndex>>,
    mut text_q: Query<(&mut Text, &mut TextColor)>,
    mut vis_q: Query<&mut Visibility>,
) {
    let now = clock.now();
    let l = strings(*lang);
    let (title, detail) = match hovered.0 {
        Some(Selected::Crew(e)) => match crews.get(e) {
            Ok((_, c, task, ..)) => (
                Some(crate::tfmt!(l.fmt_tip_crew, name = c.name)),
                Some(task_label(task, &items, &racks, l)),
            ),
            Err(_) => (None, None),
        },
        Some(Selected::Item(e)) => match items.get(e) {
            Ok((_, p, item, m, r, c, cooled)) => (
                Some(crate::tfmt!(
                    l.fmt_tip_item,
                    kind = loc::item_label(item.kind, l),
                    x = p.x,
                    y = p.y
                )),
                Some(item_status(r, c, m, cooled, &crews, now, l)),
            ),
            Err(_) => (None, None),
        },
        Some(Selected::Rack(e)) => match racks.get(e) {
            Ok((_, p, cell, _, _)) => (
                Some(crate::tfmt!(l.fmt_tip_rack, x = p.x, y = p.y)),
                Some(if cell.free() == 0 {
                    crate::tfmt!(l.rack_full, label = cell.label())
                } else {
                    let accepts = if cell.allowed.iter().all(|&a| a) {
                        l.filter_any.to_string()
                    } else {
                        crate::items::ItemKind::ALL
                            .iter()
                            .filter(|k| cell.allowed[k.index()])
                            .map(|k| loc::item_short(*k, l))
                            .collect::<Vec<_>>()
                            .join("+")
                    };
                    crate::tfmt!(
                        l.fmt_tip_rack_free,
                        label = cell.label(),
                        free = cell.free(),
                        accepts = accepts
                    )
                }),
            ),
            Err(_) => (None, None),
        },
        Some(Selected::Blueprint(e)) => match blueprints.get(e) {
            Ok((p, bp)) => (
                Some(crate::tfmt!(
                    l.fmt_tip_blueprint,
                    kind = loc::building_label(bp.kind, l),
                    x = p.x,
                    y = p.y
                )),
                Some(if bp.fully_supplied() {
                    l.tip_bp_ready.to_string()
                } else {
                    crate::tfmt!(l.fmt_tip_bp_needs, needs = bp.materials_label_loc(l))
                }),
            ),
            Err(_) => (None, None),
        },
        Some(Selected::Building(e)) => {
            if let Ok((p, b, door)) = buildings.get(e) {
                (
                    Some(crate::tfmt!(
                        l.fmt_tip_building,
                        kind = loc::building_label(b.kind, l),
                        x = p.x,
                        y = p.y
                    )),
                    Some(match door {
                        Some(d) => crate::tfmt!(
                            l.fmt_tip_door,
                            phase = loc::door_phase_label(d.phase, l),
                            mode = loc::door_mode_label(d.mode, l),
                            air = if d.sealed() {
                                l.door_airtight
                            } else {
                                l.door_air_flows
                            }
                        ),
                        None => match b.kind {
                            crate::building::BuildingKind::Rack => l.tip_rack.to_string(),
                            crate::building::BuildingKind::Fabricator => {
                                l.tip_fab_machine.to_string()
                            }
                            crate::building::BuildingKind::GasDuct => l.tip_gas_duct.to_string(),
                            _ => l.tip_structure.to_string(),
                        },
                    }),
                )
            } else {
                (None, None)
            }
        }
        None => (None, None),
    };

    let show = if title.is_some() {
        windows.single().ok().and_then(|w| w.cursor_position())
    } else {
        None
    };
    if let Ok(mut vis) = vis_q.get_mut(overlay.tooltip) {
        *vis = if show.is_some() {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    let Some((t, d)) = title.zip(detail) else {
        return;
    };
    let Some(cursor) = show else {
        return;
    };
    if let Ok((mut text, _)) = text_q.get_mut(overlay.tooltip_title) {
        text.0 = t;
    }
    if let Ok((mut text, mut color)) = text_q.get_mut(overlay.tooltip_detail) {
        text.0 = d.clone();
        color.0 = if d.starts_with(l.item_unreachable)
            || d.ends_with(strings(*lang).storage_full_suffix.trim())
        {
            Color::srgb(1.0, 0.55, 0.45)
        } else {
            Color::srgb(0.72, 0.76, 0.82)
        };
    }
    // Keep the card on screen.
    if let Ok(mut node) = node_q.get_mut(overlay.tooltip) {
        let x = (cursor.x + 16.0).min(1400.0 - TOOLTIP_W);
        let y = (cursor.y + 18.0).min(760.0);
        node.left = Val::Px(x);
        node.top = Val::Px(y);
    }
}

/// Atmosphere hover card: while the Atmosphere overlay is active and no
/// entity is under the cursor, take over the shared tooltip with the per-tile
/// gas state (pressure / temperature / composition / compartment). Runs after
/// `tooltip_system`, so an entity card always wins.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
pub fn atmosphere_tooltip_system(
    windows: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    lang: Res<Lang>,
    hovered: Res<Hovered>,
    overlay: Res<crate::OverlayMode>,
    map: Res<crate::map::ShipMap>,
    atmo: Res<crate::atmosphere::AtmosphereGrid>,
    thermal: Res<crate::thermal::ThermalGrid>,
    comps: Res<crate::airtight::Compartments>,
    overlay_res: Res<Overlay>,
    mut node_q: Query<&mut Node, With<ZIndex>>,
    mut text_q: Query<(&mut Text, &mut TextColor)>,
    mut vis_q: Query<&mut Visibility>,
) {
    if *overlay != crate::OverlayMode::Atmosphere || hovered.0.is_some() {
        return;
    }
    let Some(cursor) = windows.single().ok().and_then(|w| w.cursor_position()) else {
        return;
    };
    let Some((cam, cam_gt)) = camera.single().ok() else {
        return;
    };
    let Some(world) = cam.viewport_to_world_2d(cam_gt, cursor).ok() else {
        return;
    };
    let Some(p) = map.tile_at_world(world) else {
        return;
    };
    let mix = atmo.mixture_at(p);
    let total = mix.total();
    let i = atmo.idx(p);
    let temp = thermal.amb[i];
    let solid = matches!(
        map.tile(p),
        Some(crate::map::Tile::Wall) | Some(crate::map::Tile::BuiltWall)
    );
    let l = strings(*lang);
    let (title, detail, warn) = if solid {
        (
            crate::tfmt!(l.fmt_tip_compartment, x = p.x, y = p.y),
            l.tip_solid.to_string(),
            false,
        )
    } else if total <= 0.01 {
        (
            crate::tfmt!(l.fmt_tip_atmo_title, x = p.x, y = p.y),
            crate::tfmt!(
                l.fmt_tip_vacuum,
                t = format!("{temp:.1}"),
                r = region_label(&comps, p)
            ),
            true,
        )
    } else {
        let p_total = crate::atmosphere::pressure(total, temp);
        let o2pp = crate::atmosphere::partial_pressure(mix.mol[0], total, temp);
        let warn = p_total < crate::atmosphere::LOW_PRESSURE_KPA
            || o2pp < crate::atmosphere::O2_SAFE_KPA
            || crate::atmosphere::partial_pressure(mix.mol[2], total, temp)
                > crate::atmosphere::CO2_HIGH_KPA
            || crate::atmosphere::partial_pressure(mix.mol[3], total, temp)
                > crate::atmosphere::POLLUTANT_HIGH_KPA;
        (
            crate::tfmt!(l.fmt_tip_atmo_title, x = p.x, y = p.y),
            crate::tfmt!(
                l.fmt_tip_gas,
                p = format!("{p_total:.1}"),
                t = format!("{temp:.1}"),
                o2 = format!("{o2pp:.1}"),
                o2p = format!(
                    "{:.0}",
                    mix.fraction(crate::atmosphere::Species::O2) * 100.0
                ),
                inert = format!(
                    "{:.0}",
                    mix.fraction(crate::atmosphere::Species::Inert) * 100.0
                ),
                co2 = format!(
                    "{:.1}",
                    mix.fraction(crate::atmosphere::Species::Co2) * 100.0
                ),
                pol = format!(
                    "{:.1}",
                    mix.fraction(crate::atmosphere::Species::Pollutant) * 100.0
                ),
                r = region_label(&comps, p),
            ),
            warn,
        )
    };
    if let Ok(mut vis) = vis_q.get_mut(overlay_res.tooltip) {
        *vis = Visibility::Visible;
    }
    if let Ok((mut text, _)) = text_q.get_mut(overlay_res.tooltip_title) {
        text.0 = title;
    }
    if let Ok((mut text, mut color)) = text_q.get_mut(overlay_res.tooltip_detail) {
        text.0 = detail;
        color.0 = if warn {
            Color::srgb(1.0, 0.6, 0.45)
        } else {
            Color::srgb(0.72, 0.76, 0.82)
        };
    }
    if let Ok(mut node) = node_q.get_mut(overlay_res.tooltip) {
        let x = (cursor.x + 16.0).min(1400.0 - TOOLTIP_W);
        let y = (cursor.y + 18.0).min(760.0);
        node.left = Val::Px(x);
        node.top = Val::Px(y);
    }
}

/// Compartment number for the hover card ("—" on walls / doors / space).
fn region_label(comps: &crate::airtight::Compartments, p: TilePos) -> String {
    let r = comps.region_at(p);
    if r == crate::airtight::NO_REGION {
        "—".to_string()
    } else {
        (r + 1).to_string()
    }
}

/// Draw the box-select rectangle while the left button is dragged.
pub fn box_rect_system(
    box_select: Res<BoxSelect>,
    overlay: Res<Overlay>,
    mut node_q: Query<(&mut Node, &mut Visibility), With<ZIndex>>,
) {
    let dragging = box_select
        .anchor
        .filter(|a| box_select.current.distance(*a) > 10.0);
    if let Some(anchor) = dragging {
        let x = anchor.x.min(box_select.current.x);
        let y = anchor.y.min(box_select.current.y);
        let w = (box_select.current.x - anchor.x).abs();
        let h = (box_select.current.y - anchor.y).abs();
        if let Ok((mut node, mut vis)) = node_q.get_mut(overlay.box_rect) {
            node.left = Val::Px(x);
            node.top = Val::Px(y);
            node.width = Val::Px(w);
            node.height = Val::Px(h);
            *vis = Visibility::Visible;
        }
    } else if let Ok((_, mut vis)) = node_q.get_mut(overlay.box_rect) {
        *vis = Visibility::Hidden;
    }
}
