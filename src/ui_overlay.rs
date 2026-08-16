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
    let (title, detail) = match hovered.0 {
        Some(Selected::Crew(e)) => match crews.get(e) {
            Ok((_, c, task, ..)) => (
                Some(format!("Crew {}", c.name)),
                Some(task_label(task, &items, &racks)),
            ),
            Err(_) => (None, None),
        },
        Some(Selected::Item(e)) => match items.get(e) {
            Ok((_, p, item, m, r, c, cooled)) => (
                Some(format!("{} ({},{})", item.kind.label(), p.x, p.y)),
                Some(item_status(r, c, m, cooled, &crews, now)),
            ),
            Err(_) => (None, None),
        },
        Some(Selected::Rack(e)) => match racks.get(e) {
            Ok((_, p, cell, _, _)) => (
                Some(format!("Storage rack ({},{})", p.x, p.y)),
                Some(if cell.free() == 0 {
                    format!("{} — FULL", cell.label())
                } else {
                    format!(
                        "{} — free: {} | accepts: {}",
                        cell.label(),
                        cell.free(),
                        cell.filter_label()
                    )
                }),
            ),
            Err(_) => (None, None),
        },
        Some(Selected::Blueprint(e)) => match blueprints.get(e) {
            Ok((p, bp)) => (
                Some(format!("{} blueprint ({},{})", bp.kind.label(), p.x, p.y)),
                Some(if bp.fully_supplied() {
                    "materials complete — awaiting builder".to_string()
                } else {
                    format!("needs: {}", bp.materials_label())
                }),
            ),
            Err(_) => (None, None),
        },
        Some(Selected::Building(e)) => {
            if let Ok((p, b, door)) = buildings.get(e) {
                (
                    Some(format!("{} ({},{})", b.kind.label(), p.x, p.y)),
                    Some(match door {
                        Some(d) => format!(
                            "{} ({}) — {}",
                            d.phase.label(),
                            d.mode.label(),
                            if d.sealed() {
                                "airtight"
                            } else {
                                "air flows through"
                            }
                        ),
                        None => match b.kind {
                            crate::building::BuildingKind::Rack => "storage rack".to_string(),
                            crate::building::BuildingKind::Fabricator => {
                                "2x2 machine — select for orders".to_string()
                            }
                            _ => "player-built structure".to_string(),
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
        color.0 = if d.starts_with("Unreachable") || d.ends_with("FULL") {
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
    let (title, detail, warn) = if solid {
        (
            format!("Structure ({},{})", p.x, p.y),
            "solid — no gas volume".to_string(),
            false,
        )
    } else if total <= 0.01 {
        (
            format!("Atmosphere ({},{})", p.x, p.y),
            format!(
                "VACUUM — 0.0 kPa\nTemp {:.1}°C\nCompartment #{}",
                temp,
                region_label(&comps, p)
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
            format!("Atmosphere ({},{})", p.x, p.y),
            format!(
                "Pressure {:.1} kPa | {:.1}°C\nO2 {:.1} kPa ({:.0}%)\ninert {:.0}% | CO2 {:.1}% | pollutant {:.1}%\nCompartment #{}",
                p_total,
                temp,
                o2pp,
                mix.fraction(crate::atmosphere::Species::O2) * 100.0,
                mix.fraction(crate::atmosphere::Species::Inert) * 100.0,
                mix.fraction(crate::atmosphere::Species::Co2) * 100.0,
                mix.fraction(crate::atmosphere::Species::Pollutant) * 100.0,
                region_label(&comps, p),
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
