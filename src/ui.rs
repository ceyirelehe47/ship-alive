//! HUD: top bar (speed controls, global stats, action buttons, build tools),
//! per-crew status chips, a dynamic selection detail panel (with buttons for
//! build/deconstruct, rack filters, fabricator orders and crew work
//! priorities) and an event log.
//!
//! Every text is rebuilt each frame — at slice scale this is negligible and
//! keeps the update code trivial. All labels are ASCII because the bundled
//! default font has no CJK glyphs (recorded as a temporary behavior).
//!
//! The selection panel's *buttons* are fixed slots that get re-purposed
//! (label + OnPress action + visibility) whenever the selection or its
//! discrete state changes, so interactions never break on rebuilds.

use crate::building::{Building, BuildingKind, MarkedForDeconstruct};
use crate::crew::{Crew, CrewTask, HaulPhase, Priority, WorkKind};
use crate::input::{BuildMode, Selected, Selection, Tool};
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

/// Desired button label (synced onto the Text child by `btn_label_system`).
#[derive(Component)]
pub struct BtnLabel(pub String);

/// Marks which speed step a button represents (for highlight state).
#[derive(Component)]
pub struct SpeedIndex(pub usize);

/// Marks which tool a build-bar button represents (for highlight state).
#[derive(Component)]
pub struct ToolIndex(pub Tool);

/// The collapsed-by-default developer toolbar visibility flag.
#[derive(Resource, Default)]
pub struct DebugBarVisible(pub bool);

/// Number of fixed text lines / button slots in the selection panel.
const SEL_LINES: usize = 12;
const SEL_BTNS: usize = 16;

#[derive(Resource)]
pub struct Hud {
    pub speed_buttons: Vec<Entity>,
    pub stats: Entity,
    pub chips: Vec<Entity>,
    pub sel_lines: Vec<Entity>,
    pub sel_btn_row1: Entity,
    pub sel_btn_row2: Entity,
    pub sel_btns: Vec<Entity>,
    pub log_lines: Vec<Entity>,
    pub debug_row: Entity,
    pub debug_button_label: Entity,
    pub tool_hint: Entity,
    pub tool_buttons: Vec<Entity>,
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
        app.init_resource::<DebugBarVisible>();
        app.add_systems(Startup, (build_hud, crate::ui_overlay::build_overlay));
        app.add_systems(
            Update,
            (
                button_system,
                btn_label_system,
                debug_toggle_system,
                (hud_update_system, selection_panel_system).chain(),
                crate::ui_overlay::tooltip_system,
                crate::ui_overlay::box_rect_system,
            )
                .in_set(crate::Set::Sync),
        );
    }
}

fn build_hud(mut commands: Commands) {
    let mut speed_buttons = Vec::new();
    let mut stats = Entity::PLACEHOLDER;
    let mut chips = Vec::new();
    let mut sel_lines = Vec::new();
    let mut sel_btns = Vec::new();
    let mut log_lines = Vec::new();
    let mut debug_row = Entity::PLACEHOLDER;
    let mut debug_button_label = Entity::PLACEHOLDER;
    let mut tool_hint = Entity::PLACEHOLDER;
    let mut tool_buttons = Vec::new();
    let mut pending_tool_btns: Vec<(Entity, Tool)> = Vec::new();

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
                    label(row, "SHIP ALIVE", 15.0, Color::srgb(0.95, 0.85, 0.55));

                    for (i, text) in ["Pause", "1x", "2x", "4x"].iter().enumerate() {
                        speed_buttons.push(button(row, text, Action::SetSpeed { index: i }, 52.0));
                    }

                    stats = label(row, "", 14.0, Color::WHITE);

                    button(row, "Haul All [H]", Action::MarkAll, 96.0);
                    button(row, "Cancel All [C]", Action::CancelAll, 104.0);

                    let debug_btn = row
                        .spawn((
                            Button,
                            Interaction::default(),
                            OnPress(Action::ToggleDebug),
                            Node {
                                width: Val::Px(64.0),
                                height: Val::Px(26.0),
                                margin: UiRect::all(Val::Px(2.0)),
                                align_items: AlignItems::Center,
                                justify_content: JustifyContent::Center,
                                ..default()
                            },
                            BackgroundColor(BUTTON_BG),
                        ))
                        .with_children(|b| {
                            debug_button_label = label(b, "Debug", 13.0, Color::WHITE);
                        })
                        .id();
                    debug_row = debug_btn;
                });

                // Build tools row.
                bar.spawn(Node {
                    flex_direction: FlexDirection::Row,
                    column_gap: Val::Px(4.0),
                    align_items: AlignItems::Center,
                    ..default()
                })
                .with_children(|row| {
                    label(row, "BUILD:", 12.0, Color::srgb(0.6, 0.66, 0.72));
                    for kind in BuildingKind::ALL {
                        let tool = Tool::Build(kind);
                        let e = button(
                            row,
                            kind.label(),
                            Action::SetTool { tool: Some(tool) },
                            match kind {
                                BuildingKind::Fabricator => 88.0,
                                BuildingKind::Door => 56.0,
                                BuildingKind::Wall => 56.0,
                                BuildingKind::Rack => 96.0,
                            },
                        );
                        pending_tool_btns.push((e, tool));
                        tool_buttons.push(e);
                    }
                    let demo_tool = Tool::Deconstruct;
                    let e = button(
                        row,
                        "Deconstruct",
                        Action::SetTool { tool: Some(demo_tool) },
                        100.0,
                    );
                    pending_tool_btns.push((e, demo_tool));
                    tool_buttons.push(e);
                    button(
                        row,
                        "Cancel Tool [Esc]",
                        Action::SetTool { tool: None },
                        110.0,
                    );
                    tool_hint = label(row, "", 12.0, Color::srgb(0.6, 0.8, 0.65));
                });

                // Developer toolbar, hidden by default.
                debug_row = bar
                    .spawn((
                        Node {
                            flex_direction: FlexDirection::Row,
                            column_gap: Val::Px(6.0),
                            align_items: AlignItems::Center,
                            ..default()
                        },
                        Visibility::Hidden,
                    ))
                    .with_children(|row| {
                        button(row, "+Crate", Action::SpawnItem { kind: ItemKind::Crate }, 64.0);
                        button(row, "+Ore", Action::SpawnItem { kind: ItemKind::Ore }, 56.0);
                        button(row, "+Part", Action::SpawnItem { kind: ItemKind::Part }, 60.0);
                        label(
                            row,
                            "debug tools | [X] deletes the selected item",
                            11.0,
                            Color::srgb(0.55, 0.6, 0.66),
                        );
                    })
                    .id();

                label(
                    bar,
                    "Drag: mark items for hauling | Click: select | Right-drag / WASD: pan | Wheel: zoom | T: mark | B: build tools | Space/1/2/3: speed",
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
                                width: Val::Px(470.0),
                                padding: UiRect::all(Val::Px(8.0)),
                                flex_direction: FlexDirection::Column,
                                row_gap: Val::Px(2.0),
                                ..default()
                            },
                            BackgroundColor(PANEL_BG),
                        ))
                        .with_children(|p| {
                            for _ in 0..SEL_LINES {
                                sel_lines.push(label(p, "", 13.0, Color::WHITE));
                            }
                            // Two rows of re-purposable buttons.
                            for _row_idx in 0..2 {
                                p.spawn(Node {
                                    flex_direction: FlexDirection::Row,
                                    flex_wrap: FlexWrap::Wrap,
                                    column_gap: Val::Px(3.0),
                                    ..default()
                                })
                                .with_children(|r| {
                                    for _ in 0..(SEL_BTNS / 2) {
                                        sel_btns.push(
                                            r.spawn((
                                                Button,
                                                Interaction::default(),
                                                Node {
                                                    height: Val::Px(24.0),
                                                    padding: UiRect::horizontal(Val::Px(8.0)),
                                                    margin: UiRect::all(Val::Px(1.0)),
                                                    align_items: AlignItems::Center,
                                                    justify_content: JustifyContent::Center,
                                                    ..default()
                                                },
                                                BackgroundColor(BUTTON_BG),
                                                Visibility::Hidden,
                                            ))
                                            .with_children(|b| {
                                                label(b, "", 12.0, Color::WHITE);
                                            })
                                            .id(),
                                        );
                                    }
                                });
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
                            label(p, "EVENT LOG", 11.0, Color::srgb(0.5, 0.55, 0.62));
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
        sel_btn_row1: Entity::PLACEHOLDER,
        sel_btn_row2: Entity::PLACEHOLDER,
        sel_btns,
        log_lines,
        debug_row,
        debug_button_label,
        tool_hint,
        tool_buttons,
    });
    for (i, b) in speed_buttons.iter().enumerate() {
        commands.entity(*b).insert(SpeedIndex(i));
    }
    for (b, tool) in pending_tool_btns {
        commands.entity(b).insert(ToolIndex(tool));
    }
}

/// Sync re-purposed button labels onto their Text children.
fn btn_label_system(
    parents: Query<(&Children, &BtnLabel), Changed<BtnLabel>>,
    mut texts: Query<&mut Text>,
) {
    for (children, l) in parents.iter() {
        for &c in children {
            if let Ok(mut t) = texts.get_mut(c) {
                t.0 = l.0.clone();
            }
        }
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

/// Show/hide the developer toolbar. The toggle action itself is a lightweight
/// UI concern and does not touch the simulation.
fn debug_toggle_system(
    mut events: EventReader<Action>,
    mut visible: ResMut<DebugBarVisible>,
    hud: Res<Hud>,
    mut vis_q: Query<&mut Visibility>,
    mut text_q: Query<&mut Text>,
) {
    let mut toggled = false;
    for action in events.read() {
        if matches!(action, Action::ToggleDebug) {
            visible.0 = !visible.0;
            toggled = true;
        }
    }
    if !toggled {
        return;
    }
    if let Ok(mut vis) = vis_q.get_mut(hud.debug_row) {
        *vis = if visible.0 {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    if let Ok(mut text) = text_q.get_mut(hud.debug_button_label) {
        text.0 = if visible.0 { "Debug OK" } else { "Debug" }.to_string();
    }
}

/// Human-readable one-line state for a crew member.
#[allow(clippy::type_complexity)]
pub fn task_label(
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
                HaulPhase::ToItem | HaulPhase::PickingUp => {
                    let dest = match job.dest {
                        crate::crew::HaulDest::Storage => "storage".to_string(),
                        crate::crew::HaulDest::Blueprint(_) => "a blueprint".to_string(),
                        crate::crew::HaulDest::Machine(_) => "the fabricator".to_string(),
                    };
                    format!("Going to fetch {kind} for {dest}")
                }
                HaulPhase::ToDest | HaulPhase::Delivering => {
                    let dest = match job.dest {
                        crate::crew::HaulDest::Storage => job
                            .target_rack
                            .and_then(|r| racks.get(r).ok())
                            .map(|(p, _)| format!("rack at ({},{})", p.x, p.y))
                            .unwrap_or_else(|| "storage".to_string()),
                        crate::crew::HaulDest::Blueprint(_) => "blueprint".to_string(),
                        crate::crew::HaulDest::Machine(_) => "fabricator".to_string(),
                    };
                    format!("Carrying {kind} to {dest}")
                }
            }
        }
        CrewTask::Build(job) => {
            if job.phase == crate::crew::WorkPhase::Working {
                format!("Building ({:.0}s left)", job.timer.max(0.0))
            } else {
                "Going to build".to_string()
            }
        }
        CrewTask::Deconstruct(job) => {
            if job.phase == crate::crew::WorkPhase::Working {
                format!("Deconstructing ({:.0}s left)", job.timer.max(0.0))
            } else {
                "Going to deconstruct".to_string()
            }
        }
        CrewTask::Operate(job) => {
            if job.phase == crate::crew::WorkPhase::Working {
                format!("Operating fabricator ({:.0}s left)", job.timer.max(0.0))
            } else {
                "Going to operate fabricator".to_string()
            }
        }
    }
}

/// One-line status for an item, shared by the selection panel and tooltip.
#[allow(clippy::type_complexity)]
pub fn item_status(
    reserved: Option<&ReservedBy>,
    carried: Option<&CarriedBy>,
    marked: Option<&MarkedForHaul>,
    cooled: Option<&NoPathUntil>,
    crews: &Query<(Entity, &Crew, &CrewTask, &TilePos, &crate::crew::Movement)>,
    now: f64,
) -> String {
    if carried.is_some() {
        let carrier = carried
            .and_then(|c| crews.get(c.0).ok())
            .map(|(_, c, ..)| c.name.clone())
            .unwrap_or_else(|| "someone".into());
        format!("Being carried by {carrier}")
    } else if let Some(r) = reserved {
        let claimer = crews
            .get(r.0)
            .map(|(_, c, ..)| c.name.clone())
            .unwrap_or_else(|_| "a crew member".into());
        let dest = crews
            .get(r.0)
            .ok()
            .and_then(|(_, _, t, ..)| match t {
                CrewTask::Haul(j) => Some(match j.dest {
                    crate::crew::HaulDest::Storage => "storage".to_string(),
                    crate::crew::HaulDest::Blueprint(_) => "a blueprint".to_string(),
                    crate::crew::HaulDest::Machine(_) => "the fabricator".to_string(),
                }),
                _ => None,
            })
            .unwrap_or_else(|| "storage".into());
        format!("Claimed by {claimer} for {dest}")
    } else if marked.is_some() {
        "Marked for hauling".to_string()
    } else if cooled.is_some_and(|c| c.0 > now) {
        "Unreachable".to_string()
    } else {
        "On the ground".to_string()
    }
}

/// One button slot configuration for the selection panel.
struct BtnCfg {
    label: String,
    action: Action,
    active: bool,
}

impl BtnCfg {
    fn new(label: impl Into<String>, action: Action) -> Self {
        Self {
            label: label.into(),
            action,
            active: false,
        }
    }

    fn active(mut self, on: bool) -> Self {
        self.active = on;
        self
    }
}

/// Discrete selection-panel state; buttons are re-purposed when it changes.
#[derive(PartialEq, Clone, Debug, Default)]
enum SelSig {
    #[default]
    None,
    Crew {
        e: Entity,
        prio: [u8; 3],
    },
    Item {
        e: Entity,
        marked: bool,
    },
    Rack {
        e: Entity,
        demo: bool,
        allowed: [bool; 3],
    },
    Blueprint {
        e: Entity,
    },
    Building {
        e: Entity,
        kind: BuildingKind,
        demo: bool,
    },
    Fab {
        e: Entity,
        demo: bool,
        repeat: bool,
        ordered: bool,
    },
}

fn prio_code(p: &crate::crew::WorkPriorities) -> [u8; 3] {
    let enc = |p: Priority| -> u8 {
        match p {
            Priority::Disabled => 0,
            Priority::Low => 1,
            Priority::Normal => 2,
            Priority::High => 3,
        }
    };
    [enc(p.haul), enc(p.build), enc(p.operate)]
}

#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn selection_panel_system(
    hud: Res<Hud>,
    selection: Res<Selection>,
    time: Res<Time<Virtual>>,
    mut commands: Commands,
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
    racks_full: Query<(
        Entity,
        &TilePos,
        &StorageCell,
        Option<&Building>,
        Option<&MarkedForDeconstruct>,
    )>,
    blueprints: Query<(Entity, &TilePos, &crate::building::Blueprint)>,
    fabs: Query<(
        Entity,
        &TilePos,
        &crate::production::Fabricator,
        Option<&MarkedForDeconstruct>,
    )>,
    buildings: Query<
        (Entity, &TilePos, &Building, Option<&MarkedForDeconstruct>),
        (With<Building>, Without<StorageCell>),
    >,
    mut texts: Query<(&mut Text, &mut TextColor, &mut Visibility), Without<Button>>,
    mut btn_q: Query<
        (&Interaction, &mut BackgroundColor, &mut Visibility),
        (With<Button>, Without<SpeedIndex>, Without<ToolIndex>),
    >,
    mut last_sig: Local<SelSig>,
) {
    let now = time.elapsed().as_secs_f64();
    // ---- selection panel: text lines ----
    let mut lines: Vec<(String, Color)> = Vec::new();
    let sig = match selection.0 {
        None => SelSig::None,
        Some(Selected::Crew(e)) => match crews.get(e) {
            Ok((_, c, _, _, _)) => SelSig::Crew {
                e,
                prio: prio_code(&c.priorities),
            },
            Err(_) => SelSig::None,
        },
        Some(Selected::Item(e)) => match items.get(e) {
            Ok((_, _, _, m, _, _, _)) => SelSig::Item {
                e,
                marked: m.is_some(),
            },
            Err(_) => SelSig::None,
        },
        Some(Selected::Rack(e)) => match racks_full.get(e) {
            Ok((_, _, cell, _, demo)) => SelSig::Rack {
                e,
                demo: demo.is_some(),
                allowed: cell.allowed,
            },
            Err(_) => SelSig::None,
        },
        Some(Selected::Blueprint(e)) => match blueprints.get(e) {
            Ok(_) => SelSig::Blueprint { e },
            Err(_) => SelSig::None,
        },
        Some(Selected::Building(e)) => {
            if let Ok((_, _, b, demo)) = buildings.get(e) {
                SelSig::Building {
                    e,
                    kind: b.kind,
                    demo: demo.is_some(),
                }
            } else if let Ok((_, _, cell, _, demo)) = racks_full.get(e) {
                SelSig::Rack {
                    e,
                    demo: demo.is_some(),
                    allowed: cell.allowed,
                }
            } else if let Ok((_, _, f, demo)) = fabs.get(e) {
                SelSig::Fab {
                    e,
                    demo: demo.is_some(),
                    repeat: f.order.is_some_and(|o| o.repeat),
                    ordered: f.order.is_some(),
                }
            } else {
                SelSig::None
            }
        }
    };

    match selection.0 {
        Some(Selected::Crew(e)) => {
            if let Ok((_, crew, task, pos, mov)) = crews.get(e) {
                lines.push((
                    format!("Crew {} ({},{})", crew.name, pos.x, pos.y),
                    crew.tint,
                ));
                lines.push((task_label(task, &items, &racks), Color::WHITE));
                match task {
                    CrewTask::Haul(job) => {
                        let detail = match job.phase {
                            HaulPhase::ToItem | HaulPhase::PickingUp => items
                                .get(job.item)
                                .ok()
                                .map(|(_, p, ..)| format!("Target item at ({},{})", p.x, p.y))
                                .unwrap_or_else(|| "Target item gone".into()),
                            _ => match job.dest {
                                crate::crew::HaulDest::Storage => job
                                    .target_rack
                                    .and_then(|r| racks.get(r).ok())
                                    .map(|(p, s)| {
                                        format!("Deliver to rack ({},{}) [{}]", p.x, p.y, s.label())
                                    })
                                    .unwrap_or_else(|| "No rack chosen".into()),
                                crate::crew::HaulDest::Blueprint(_) => {
                                    "Deliver to blueprint".into()
                                }
                                crate::crew::HaulDest::Machine(_) => "Load into fabricator".into(),
                            },
                        };
                        lines.push((detail, Color::srgb(0.75, 0.78, 0.82)));
                    }
                    CrewTask::Build(_) => lines.push((
                        "Target: construction blueprint".into(),
                        Color::srgb(0.75, 0.78, 0.82),
                    )),
                    CrewTask::Deconstruct(_) => lines.push((
                        "Target: building to tear down".into(),
                        Color::srgb(0.75, 0.78, 0.82),
                    )),
                    CrewTask::Operate(_) => {
                        lines.push(("Target: fabricator".into(), Color::srgb(0.75, 0.78, 0.82)))
                    }
                    CrewTask::Idle(cause) => {
                        lines.push((cause.label(), Color::srgb(0.7, 0.74, 0.8)));
                    }
                }
                lines.push((
                    format!(
                        "Hauled: {} | Built: {} | Operated: {} | Path: {}",
                        crew.delivered,
                        crew.built,
                        crew.operated,
                        mov.path.len()
                    ),
                    Color::srgb(0.6, 0.66, 0.72),
                ));
                lines.push((
                    "Work priorities (below) decide which jobs this crew takes first.".to_string(),
                    Color::srgb(0.6, 0.66, 0.72),
                ));
            }
        }
        Some(Selected::Item(e)) => {
            if let Ok((_, pos, item, marked, reserved, carried, cooled)) = items.get(e) {
                lines.push((
                    format!("Item: {} ({},{})", item.kind.label(), pos.x, pos.y),
                    Color::srgb(0.95, 0.85, 0.55),
                ));
                lines.push((
                    item_status(reserved, carried, marked, cooled, &crews, now),
                    Color::WHITE,
                ));
                if let Some(c) = cooled {
                    if c.0 > now {
                        lines.push((
                            format!("Unreachable (retry in {:.0}s)", c.0 - now),
                            Color::srgb(1.0, 0.45, 0.4),
                        ));
                    }
                }
                lines.push((
                    "[T] toggle haul mark".to_string(),
                    Color::srgb(0.6, 0.66, 0.72),
                ));
            }
        }
        Some(Selected::Rack(e)) => {
            if let Ok((_, pos, cell, _, demo)) = racks_full.get(e) {
                lines.push((
                    format!("Storage rack ({},{})", pos.x, pos.y),
                    Color::srgb(0.6, 0.9, 0.8),
                ));
                let counts = ItemKind::ALL
                    .iter()
                    .map(|k| format!("{}: {}", k.label(), cell.counts[k.index()]))
                    .collect::<Vec<_>>()
                    .join(" | ");
                lines.push((counts, Color::WHITE));
                lines.push((
                    format!(
                        "Free slots: {} | Accepts: {}",
                        cell.free(),
                        cell.filter_label()
                    ),
                    if cell.free() == 0 {
                        Color::srgb(1.0, 0.45, 0.4)
                    } else {
                        Color::WHITE
                    },
                ));
                if demo.is_some() {
                    lines.push((
                        "MARKED FOR DECONSTRUCTION".into(),
                        Color::srgb(1.0, 0.7, 0.25),
                    ));
                }
                lines.push((
                    "Toggle which kinds this rack accepts (below).".to_string(),
                    Color::srgb(0.6, 0.66, 0.72),
                ));
            }
        }
        Some(Selected::Blueprint(e)) => {
            if let Ok((_, pos, bp)) = blueprints.get(e) {
                lines.push((
                    format!("Blueprint: {} at ({},{})", bp.kind.label(), pos.x, pos.y),
                    Color::srgb(0.55, 0.85, 1.0),
                ));
                lines.push((format!("Materials: {}", bp.materials_label()), Color::WHITE));
                for (kind, miss) in bp.missing_list() {
                    lines.push((
                        format!("  waiting for {miss} more {}", kind.label()),
                        Color::srgb(1.0, 0.75, 0.4),
                    ));
                }
                if bp.fully_supplied() {
                    lines.push((
                        if bp.progress > 0.0 {
                            format!("Under construction — {}%", (bp.progress * 100.0) as u32)
                        } else {
                            "Fully supplied — waiting for a builder".to_string()
                        },
                        Color::srgb(0.55, 0.9, 0.6),
                    ));
                }
            }
        }
        Some(Selected::Building(e)) => {
            if let Ok((_, pos, b, demo)) = buildings.get(e) {
                lines.push((
                    format!("{} ({},{})", b.kind.label(), pos.x, pos.y),
                    Color::srgb(0.85, 0.85, 0.9),
                ));
                if demo.is_some() {
                    lines.push((
                        if b.demo_progress > 0.0 {
                            format!("DECONSTRUCTING — {}%", (b.demo_progress * 100.0) as u32)
                        } else {
                            "MARKED FOR DECONSTRUCTION — waiting for a crew".to_string()
                        },
                        Color::srgb(1.0, 0.7, 0.25),
                    ));
                } else {
                    lines.push((
                        "Use Deconstruct (BUILD bar) to tear this down.".to_string(),
                        Color::srgb(0.6, 0.66, 0.72),
                    ));
                }
            } else if let Ok((_, pos, f, demo)) = fabs.get(e) {
                let state = f.state();
                lines.push((
                    format!("Fabricator ({},{})", pos.x, pos.y),
                    Color::srgb(0.75, 0.8, 1.0),
                ));
                lines.push((
                    format!(
                        "State: {} | in: {} ore | out: {} parts",
                        state.label(),
                        f.input[ItemKind::Ore.index()],
                        f.output[ItemKind::Part.index()]
                    ),
                    match state {
                        crate::production::MachineState::Working => Color::srgb(0.55, 1.0, 0.65),
                        crate::production::MachineState::OutputBlocked => {
                            Color::srgb(1.0, 0.5, 0.4)
                        }
                        _ => Color::WHITE,
                    },
                ));
                let order = match &f.order {
                    Some(o) if o.repeat => "Repeat (endless)".to_string(),
                    Some(o) => format!("{} batch(es) left", o.batches),
                    None => "No order".to_string(),
                };
                lines.push((order, Color::srgb(0.75, 0.78, 0.82)));
                if state == crate::production::MachineState::Working {
                    lines.push((
                        format!("Progress: {}%", (f.progress * 100.0) as u32),
                        Color::srgb(0.55, 1.0, 0.65),
                    ));
                }
                if demo.is_some() {
                    lines.push((
                        "MARKED FOR DECONSTRUCTION".into(),
                        Color::srgb(1.0, 0.7, 0.25),
                    ));
                }
                lines.push((
                    "Recipe: 2 Asteroid Ore -> 1 Machinery Part (6s)".to_string(),
                    Color::srgb(0.6, 0.66, 0.72),
                ));
            }
        }
        None => {
            lines.push((
                "Nothing selected — click a crew member, item, rack, building or blueprint."
                    .to_string(),
                Color::srgb(0.6, 0.66, 0.72),
            ));
            lines.push((
                "Build with the BUILD bar, then watch the crew deliver materials and construct."
                    .to_string(),
                Color::srgb(0.6, 0.66, 0.72),
            ));
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

    // ---- selection panel: re-purposable buttons ----
    if *last_sig != sig {
        *last_sig = sig.clone();
        let cfgs: Vec<BtnCfg> = match &sig {
            SelSig::None => Vec::new(),
            SelSig::Crew { e, .. } => {
                let mut v = Vec::new();
                if let Ok((_, crew, _, _, _)) = crews.get(*e) {
                    for wk in WorkKind::ALL {
                        for p in Priority::ALL {
                            v.push(
                                BtnCfg::new(
                                    format!("{}:{}", wk.label(), p.label()),
                                    Action::SetPriority {
                                        crew: *e,
                                        work: wk,
                                        level: p,
                                    },
                                )
                                .active(crew.priorities.get(wk) == p),
                            );
                        }
                    }
                }
                v
            }
            SelSig::Item { e, marked } => vec![BtnCfg::new(
                if *marked {
                    "Unmark haul [T]"
                } else {
                    "Mark for haul [T]"
                },
                Action::ToggleMark { item: *e },
            )],
            SelSig::Rack {
                e, allowed, demo, ..
            } => {
                let mut v = Vec::new();
                for k in ItemKind::ALL {
                    v.push(BtnCfg::new(
                        format!(
                            "{}:{}",
                            if allowed[k.index()] { "allow" } else { "deny" },
                            k.label()
                        ),
                        Action::SetRackFilter {
                            rack: *e,
                            kind: k,
                            allowed: !allowed[k.index()],
                        },
                    ));
                }
                if *demo {
                    v.push(BtnCfg::new(
                        "Cancel deconstruction",
                        Action::UnmarkDeconstruct { building: *e },
                    ));
                } else {
                    v.push(BtnCfg::new(
                        "Deconstruct",
                        Action::MarkDeconstruct { building: *e },
                    ));
                }
                v
            }
            SelSig::Blueprint { e } => vec![BtnCfg::new(
                "Cancel blueprint (refund)",
                Action::CancelBlueprint { blueprint: *e },
            )],
            SelSig::Building { e, demo, .. } => {
                if *demo {
                    vec![BtnCfg::new(
                        "Cancel deconstruction",
                        Action::UnmarkDeconstruct { building: *e },
                    )]
                } else {
                    vec![BtnCfg::new(
                        "Deconstruct",
                        Action::MarkDeconstruct { building: *e },
                    )]
                }
            }
            SelSig::Fab {
                e,
                demo,
                repeat,
                ordered,
            } => {
                let mut v = vec![
                    BtnCfg::new(
                        "+1 batch",
                        Action::FabAddOrder {
                            fab: *e,
                            batches: 1,
                        },
                    ),
                    BtnCfg::new(
                        "+5",
                        Action::FabAddOrder {
                            fab: *e,
                            batches: 5,
                        },
                    ),
                    BtnCfg::new(
                        if *repeat { "Repeat: ON" } else { "Repeat: OFF" },
                        Action::FabRepeat { fab: *e },
                    )
                    .active(*repeat),
                ];
                if *ordered {
                    v.push(BtnCfg::new(
                        "Clear order",
                        Action::FabClearOrder { fab: *e },
                    ));
                }
                if *demo {
                    v.push(BtnCfg::new(
                        "Cancel deconstruction",
                        Action::UnmarkDeconstruct { building: *e },
                    ));
                } else {
                    v.push(BtnCfg::new(
                        "Deconstruct",
                        Action::MarkDeconstruct { building: *e },
                    ));
                }
                v
            }
        };
        for (i, slot) in hud.sel_btns.iter().enumerate() {
            let Ok((_, mut bg, mut vis)) = btn_q.get_mut(*slot) else {
                continue;
            };
            if i < cfgs.len() {
                let cfg = &cfgs[i];
                commands
                    .entity(*slot)
                    .insert(OnPress(cfg.action))
                    .insert(BtnLabel(cfg.label.clone()));
                bg.0 = if cfg.active { BUTTON_ACTIVE } else { BUTTON_BG };
                *vis = Visibility::Visible;
            } else {
                *vis = Visibility::Hidden;
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
    stats: Res<crate::stats::Stats>,
    log: Res<EventLog>,
    build_mode: Res<BuildMode>,
    mut speed_btn_q: Query<(&SpeedIndex, &Interaction, &mut BackgroundColor), With<SpeedIndex>>,
    mut tool_btn_q: Query<
        (&ToolIndex, &Interaction, &mut BackgroundColor),
        (With<ToolIndex>, Without<SpeedIndex>),
    >,
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
    mut texts: Query<(&mut Text, &mut TextColor, &mut Visibility), Without<Button>>,
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

    // ---- build tool buttons + hint ----
    for (ti, interaction, mut bg) in tool_btn_q.iter_mut() {
        let active = build_mode.0 == Some(ti.0);
        bg.0 = if active {
            BUTTON_ACTIVE
        } else if *interaction == Interaction::Hovered {
            BUTTON_HOVER
        } else {
            BUTTON_BG
        };
    }
    if let Ok((mut text, _, _)) = texts.get_mut(hud.tool_hint) {
        text.0 = match build_mode.0 {
            Some(Tool::Build(kind)) => format!(
                "Placing {} — click the map ({:?} parts) | Esc to cancel",
                kind.label(),
                crate::building::def(kind).cost
            ),
            Some(Tool::Deconstruct) => {
                "Click a building to mark it for deconstruction | Esc to cancel".to_string()
            }
            None => String::new(),
        };
    }

    // ---- stats line ----
    let marked = items.iter().filter(|(.., m, _, _, _)| m.is_some()).count();
    let stored: u32 = racks.iter().map(|(_, s)| s.stored()).sum();
    let cap: u32 = racks.iter().map(|(_, s)| s.capacity).sum();
    let idle = crews
        .iter()
        .filter(|(_, _, t, ..)| matches!(t, CrewTask::Idle(_)))
        .count();
    let secs = now as i64;
    let clock = format!("{:02}:{:02}", secs / 60, secs % 60);
    if let Ok((mut text, mut color, _)) = texts.get_mut(hud.stats) {
        text.0 = format!(
            "Marked: {marked} | Storage: {stored}/cap{} | Parts made: {} | Built: {} | Crew idle: {}/4 | {clock} | {}",
            "",
            stats.produced,
            stats.built,
            idle,
            speed.label(),
        );
        // Show capacity summary with actual number.
        text.0 = text.0.replacen("cap", &format!("{cap}"), 1);
        color.0 = if cap == stored {
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
            let mut line = format!(
                "[{}] {}: {}",
                if mov.path.is_empty() { "·" } else { ">" },
                crew.name,
                task_label(task, &items, &racks)
            );
            line.push_str(&format!(
                "  (hauled {} built {} ops {})",
                crew.delivered, crew.built, crew.operated
            ));
            text.0 = line;
            color.0 = crew.tint;
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
