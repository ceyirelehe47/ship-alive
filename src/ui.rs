//! HUD: top bar (speed controls, global stats, action buttons), per-crew
//! status chips, a selection detail panel and an event log.
//!
//! Every text is rebuilt each frame — at slice-0 scale this is negligible and
//! keeps the update code trivial. All labels are ASCII because the bundled
//! default font has no CJK glyphs (recorded as a temporary behavior).

use crate::crew::{Crew, CrewTask, HaulPhase};
use crate::input::{Selected, Selection};
use crate::items::{CarriedBy, Item, ItemKind, MarkedForHaul, NoPathUntil, ReservedBy};
use crate::jobs::Action;
use crate::log::{EventLog, LogKind};
use crate::map::TilePos;
use crate::storage::StorageCell;
use crate::time_ctrl::GameSpeed;
use bevy::prelude::*;

const PANEL_BG: Color = Color::srgba(0.06, 0.08, 0.11, 0.88);
const BUTTON_BG: Color = Color::srgba(0.22, 0.27, 0.34, 1.0);
const BUTTON_ACTIVE: Color = Color::srgba(0.95, 0.72, 0.20, 1.0);
const BUTTON_HOVER: Color = Color::srgba(0.34, 0.40, 0.48, 1.0);

/// Component linking a button to the action it fires when pressed.
#[derive(Component)]
pub struct OnPress(pub Action);

/// Marks which speed step a button represents (for highlight state).
#[derive(Component)]
pub struct SpeedIndex(pub usize);

#[derive(Resource)]
pub struct Hud {
    pub speed_buttons: Vec<Entity>,
    pub stats: Entity,
    pub chips: Vec<Entity>,
    pub sel_lines: Vec<Entity>,
    pub log_lines: Vec<Entity>,
}

fn label(parent: &mut ChildSpawnerCommands, text: &str, size: f32, color: Color) -> Entity {
    parent
        .spawn((
            Text::new(text),
            TextFont {
                font_size: size,
                ..default()
            },
            TextColor(color),
        ))
        .id()
}

fn button(parent: &mut ChildSpawnerCommands, text: &str, action: Action, width: f32) -> Entity {
    parent
        .spawn((
            Button,
            Interaction::default(),
            OnPress(action),
            Node {
                width: Val::Px(width),
                height: Val::Px(26.0),
                margin: UiRect::all(Val::Px(2.0)),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(BUTTON_BG),
        ))
        .with_children(|b| {
            label(b, text, 13.0, Color::WHITE);
        })
        .id()
}

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, build_hud);
        app.add_systems(
            Update,
            (button_system, hud_update_system).in_set(crate::Set::Sync),
        );
    }
}

fn build_hud(mut commands: Commands) {
    let mut speed_buttons = Vec::new();
    let mut stats = Entity::PLACEHOLDER;
    let mut chips = Vec::new();
    let mut sel_lines = Vec::new();
    let mut log_lines = Vec::new();

    commands
        .spawn(Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::SpaceBetween,
            ..default()
        })
        .with_children(|root| {
            // ---- top bar ----
            root.spawn((
                Interaction::default(),
                Node {
                    width: Val::Percent(100.0),
                    padding: UiRect::all(Val::Px(6.0)),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(2.0),
                    ..default()
                },
                BackgroundColor(PANEL_BG),
            ))
            .with_children(|bar| {
                bar.spawn(Node {
                    flex_direction: FlexDirection::Row,
                    column_gap: Val::Px(6.0),
                    align_items: AlignItems::Center,
                    flex_wrap: FlexWrap::Wrap,
                    ..default()
                })
                .with_children(|row| {
                    label(row, "SHIP ALIVE — Slice 0", 15.0, Color::srgb(0.95, 0.85, 0.55));

                    for (i, text) in ["Pause", "1x", "2x", "4x"].iter().enumerate() {
                        speed_buttons.push(button(row, text, Action::SetSpeed { index: i }, 52.0));
                    }

                    stats = label(row, "", 14.0, Color::WHITE);

                    button(row, "Haul All [H]", Action::MarkAll, 96.0);
                    button(row, "Cancel All [C]", Action::CancelAll, 104.0);
                    button(row, "+Crate", Action::SpawnItem { kind: ItemKind::Crate }, 64.0);
                    button(row, "+Ore", Action::SpawnItem { kind: ItemKind::Ore }, 56.0);
                    button(row, "+Part", Action::SpawnItem { kind: ItemKind::Part }, 60.0);
                });

                label(
                    bar,
                    "Click: select crew/item/rack | T: mark/unmark item | X: delete item | Space/1/2/3: speed | WASD/arrows: pan | wheel: zoom",
                    11.0,
                    Color::srgb(0.6, 0.66, 0.72),
                );
            });

            // ---- bottom ----
            root.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                ..default()
            })
            .with_children(|bottom| {
                // crew chips
                bottom
                    .spawn((
                        Interaction::default(),
                        Node {
                            flex_direction: FlexDirection::Row,
                            column_gap: Val::Px(6.0),
                            ..default()
                        },
                    ))
                    .with_children(|row| {
                        for _ in 0..4 {
                            chips.push(
                                row.spawn((
                                    Text::new(""),
                                    TextFont {
                                        font_size: 13.0,
                                        ..default()
                                    },
                                    TextColor(Color::WHITE),
                                    Node {
                                        padding: UiRect::all(Val::Px(4.0)),
                                        ..default()
                                    },
                                    BackgroundColor(PANEL_BG),
                                ))
                                .id(),
                            );
                        }
                    });

                bottom
                    .spawn(Node {
                        flex_direction: FlexDirection::Row,
                        column_gap: Val::Px(6.0),
                        align_items: AlignItems::FlexEnd,
                        ..default()
                    })
                    .with_children(|row| {
                        // selection panel
                        row.spawn((
                            Interaction::default(),
                            Node {
                                width: Val::Px(430.0),
                                padding: UiRect::all(Val::Px(8.0)),
                                flex_direction: FlexDirection::Column,
                                row_gap: Val::Px(2.0),
                                ..default()
                            },
                            BackgroundColor(PANEL_BG),
                        ))
                        .with_children(|p| {
                            for _ in 0..4 {
                                sel_lines.push(label(p, "", 13.0, Color::WHITE));
                            }
                        });

                        // event log
                        row.spawn((
                            Interaction::default(),
                            Node {
                                flex_grow: 1.0,
                                padding: UiRect::all(Val::Px(8.0)),
                                flex_direction: FlexDirection::Column,
                                row_gap: Val::Px(1.0),
                                ..default()
                            },
                            BackgroundColor(PANEL_BG),
                        ))
                        .with_children(|p| {
                            for _ in 0..EventLog::VISIBLE {
                                log_lines.push(label(p, "", 12.0, Color::srgb(0.75, 0.78, 0.82)));
                            }
                        });
                    });
            });
        });

    commands.insert_resource(Hud {
        speed_buttons: speed_buttons.clone(),
        stats,
        chips,
        sel_lines,
        log_lines,
    });
    for (i, b) in speed_buttons.iter().enumerate() {
        commands.entity(*b).insert(SpeedIndex(i));
    }
}

/// Fire button presses as actions.
fn button_system(
    interactions: Query<(&Interaction, &OnPress), Changed<Interaction>>,
    mut actions: EventWriter<Action>,
) {
    for (interaction, on_press) in interactions.iter() {
        if *interaction == Interaction::Pressed {
            actions.write(on_press.0);
        }
    }
}

/// Human-readable one-line state for a crew member.
#[allow(clippy::type_complexity)]
fn task_label(
    task: &CrewTask,
    items: &Query<
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
    racks: &Query<(&TilePos, &StorageCell), With<StorageCell>>,
) -> String {
    match task {
        CrewTask::Idle(cause) => cause.label(),
        CrewTask::Haul(job) => {
            let kind = items
                .get(job.item)
                .map(|(_, _, i, ..)| i.kind.label())
                .unwrap_or("item");
            match job.phase {
                HaulPhase::ToItem | HaulPhase::PickingUp => format!("Going to item ({kind})"),
                HaulPhase::ToStorage | HaulPhase::Storing => {
                    let rack = job
                        .target_rack
                        .and_then(|r| racks.get(r).ok())
                        .map(|(p, _)| format!("rack at ({},{})", p.x, p.y))
                        .unwrap_or_else(|| "storage".to_string());
                    format!("Carrying {kind} to {rack}")
                }
            }
        }
    }
}

#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn hud_update_system(
    hud: Res<Hud>,
    speed: Res<GameSpeed>,
    time: Res<Time<Virtual>>,
    log: Res<EventLog>,
    selection: Res<Selection>,
    mut speed_btn_q: Query<(&SpeedIndex, &Interaction, &mut BackgroundColor), With<OnPress>>,
    crews: Query<(Entity, &Crew, &CrewTask, &TilePos, &crate::crew::Movement)>,
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
    racks: Query<(&TilePos, &StorageCell), With<StorageCell>>,
    mut texts: Query<(&mut Text, &mut TextColor, &mut Visibility)>,
) {
    let now = time.elapsed().as_secs_f64();

    // ---- speed buttons ----
    for (idx, interaction, mut bg) in speed_btn_q.iter_mut() {
        bg.0 = if speed.index == idx.0 {
            BUTTON_ACTIVE
        } else if *interaction == Interaction::Hovered {
            BUTTON_HOVER
        } else {
            BUTTON_BG
        };
    }

    // ---- stats line ----
    let marked = items.iter().filter(|(.., m, _, _, _)| m.is_some()).count();
    let stored: u32 = racks.iter().map(|(_, s)| s.stored()).sum();
    let cap: u32 = racks.iter().map(|(_, s)| s.capacity).sum();
    let idle = crews.iter().filter(|(_, _, t, ..)| matches!(t, CrewTask::Idle(_))).count();
    let secs = now as i64;
    let clock = format!("{:02}:{:02}", secs / 60, secs % 60);
    if let Ok((mut text, mut color, _)) = texts.get_mut(hud.stats) {
        text.0 = format!(
            "Marked: {marked} | Storage: {stored}/{cap}{} | Crew idle: {}/{} | {clock} | {}",
            if cap - stored == 0 { " FULL" } else { "" },
            idle,
            crews.iter().count(),
            speed.label(),
        );
        color.0 = if cap - stored == 0 {
            Color::srgb(1.0, 0.45, 0.4)
        } else {
            Color::WHITE
        };
    }

    // ---- crew chips ----
    for (i, chip) in hud.chips.iter().enumerate() {
        let Some((_, crew, task, _, mov)) = crews.iter().nth(i) else {
            if let Ok((_, _, mut vis)) = texts.get_mut(*chip) {
                *vis = Visibility::Hidden;
            }
            continue;
        };
        if let Ok((mut text, mut color, mut vis)) = texts.get_mut(*chip) {
            *vis = Visibility::Visible;
            let mut line = format!("[{}] {}: {}", if mov.path.is_empty() { "·" } else { ">" }, crew.name, task_label(task, &items, &racks));
            line.push_str(&format!("  (delivered {})", crew.delivered));
            text.0 = line;
            color.0 = crew.tint;
        }
    }

    // ---- selection panel ----
    let mut lines: Vec<(String, Color)> = Vec::new();
    match selection.0 {
        Some(Selected::Crew(e)) => {
            if let Ok((_, crew, task, pos, mov)) = crews.get(e) {
                lines.push((format!("Crew {} ({},{})", crew.name, pos.x, pos.y), crew.tint));
                lines.push((task_label(task, &items, &racks), Color::WHITE));
                if let CrewTask::Haul(job) = task {
                    let detail = match job.phase {
                        HaulPhase::ToItem | HaulPhase::PickingUp => items
                            .get(job.item)
                            .ok()
                            .map(|(_, p, ..)| format!("Target item at ({},{})", p.x, p.y))
                            .unwrap_or_else(|| "Target item gone".into()),
                        _ => job
                            .target_rack
                            .and_then(|r| racks.get(r).ok())
                            .map(|(p, s)| format!("Deliver to rack ({},{}) [{}]", p.x, p.y, s.label()))
                            .unwrap_or_else(|| "No rack chosen".into()),
                    };
                    lines.push((detail, Color::srgb(0.75, 0.78, 0.82)));
                }
                lines.push((format!("Delivered: {} | Tiles left: {}", crew.delivered, mov.path.len()), Color::srgb(0.6, 0.66, 0.72)));
            }
        }
        Some(Selected::Item(e)) => {
            if let Ok((_, pos, item, marked, reserved, carried, cooled)) = items.get(e) {
                lines.push((format!("Item: {} ({},{})", item.kind.label(), pos.x, pos.y), Color::srgb(0.95, 0.85, 0.55)));
                let status = if carried.is_some() {
                    "being carried".to_string()
                } else if let Some(r) = reserved {
                    format!("claimed by crew #{}", r.0.index())
                } else if marked.is_some() {
                    "marked for hauling".to_string()
                } else {
                    "on the ground".to_string()
                };
                lines.push((status, Color::WHITE));
                if let Some(c) = cooled {
                    if c.0 > now {
                        lines.push((format!("Unreachable (retry in {:.0}s)", c.0 - now), Color::srgb(1.0, 0.45, 0.4)));
                    }
                }
                lines.push(("[T] toggle haul mark | [X] delete item".to_string(), Color::srgb(0.6, 0.66, 0.72)));
            }
        }
        Some(Selected::Rack(e)) => {
            if let Ok((pos, cell)) = racks.get(e) {
                lines.push((format!("Storage rack ({},{})", pos.x, pos.y), Color::srgb(0.6, 0.9, 0.8)));
                let counts = ItemKind::ALL
                    .iter()
                    .map(|k| format!("{}: {}", k.label(), cell.counts[k.index()]))
                    .collect::<Vec<_>>()
                    .join(" | ");
                lines.push((counts, Color::WHITE));
                lines.push((format!("Free slots: {}", cell.free()), if cell.free() == 0 { Color::srgb(1.0, 0.45, 0.4) } else { Color::WHITE }));
            }
        }
        None => {
            lines.push(("Nothing selected — click a crew member, item or rack.".to_string(), Color::srgb(0.6, 0.66, 0.72)));
            lines.push(("Use [H] Haul All to put the crew to work.".to_string(), Color::srgb(0.6, 0.66, 0.72)));
        }
    }

    for (i, line) in hud.sel_lines.iter().enumerate() {
        if let Ok((mut text, mut color, mut vis)) = texts.get_mut(*line) {
            if i < lines.len() {
                *vis = Visibility::Visible;
                text.0 = lines[i].0.clone();
                color.0 = lines[i].1;
            } else {
                *vis = Visibility::Hidden;
            }
        }
    }

    // ---- event log ----
    let start = log.entries.len().saturating_sub(EventLog::VISIBLE);
    for (i, line_e) in hud.log_lines.iter().enumerate() {
        let entry_idx = start + i;
        if let Ok((mut text, mut color, _)) = texts.get_mut(*line_e) {
            if entry_idx < log.entries.len() {
                let e = &log.entries[entry_idx];
                text.0 = format!("[{:>4}s] {}", e.time as i64, e.text);
                color.0 = match e.kind {
                    LogKind::Info => Color::srgb(0.68, 0.72, 0.78),
                    LogKind::Job => Color::srgb(0.65, 0.9, 0.7),
                    LogKind::Fail => Color::srgb(1.0, 0.55, 0.45),
                };
            } else {
                text.0 = String::new();
            }
        }
    }
}
