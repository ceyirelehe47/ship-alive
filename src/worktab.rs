//! The WORK tab (Slice 7): a RimWorld-style work priority matrix.
//!
//! Rows are work types (Haul / Build / Operate), columns are crew members.
//! Each cell shows the crew's CURRENT tier and clicking it cycles the tier
//! (Off → Low → Normal → High → Off) through `Action::SetPriority`, which
//! also wakes an idle scanner immediately. The header row shows each crew's
//! live activity; clicking a name selects that crew. Priorities only steer
//! which job a crew takes NEXT — running jobs always finish (no preemption),
//! matching the RimWorld/ONI feel.

use crate::crew::{Crew, CrewTask, Priority, WorkKind};
use crate::input::{Selected, Selection};
use crate::items::{CarriedBy, Item, MarkedForHaul, NoPathUntil, ReservedBy};
use crate::jobs::Action;
use crate::map::TilePos;
use crate::storage::StorageCell;
use crate::ui::{label, OnPress, BUTTON_ACTIVE, BUTTON_BG, BUTTON_HOVER, PANEL_BG};
use bevy::color::Mix;
use bevy::prelude::*;

/// Crew columns pooled at startup (the starter ship has 4).
pub const MAX_CREW_COLS: usize = 8;

// ---- layout constants -------------------------------------------------------
const COL_W: f32 = 78.0;
const CORNER_W: f32 = 172.0;
const BTN_H: f32 = 24.0;

/// Whether the WORK tab is currently shown ([Tab] / top-bar button).
#[derive(Resource, Default)]
pub struct WorkTabVisible(pub bool);

/// Marks the WORK tab root node (visibility driver).
#[derive(Component)]
pub struct WorkTabRoot;

/// Entity handles for the WORK tab's pooled widgets.
#[derive(Resource)]
pub struct WorkTabHud {
    pub cols: Vec<WorkTabCol>,
}

/// One crew column: name header button, live-activity line, stats line and
/// one priority cell per work type.
pub struct WorkTabCol {
    pub name_btn: Entity,
    pub name_label: Entity,
    pub cur: Entity,
    pub stats: Entity,
    /// (cell button, cell label) per `WorkKind::ALL` index.
    pub cells: [(Entity, Entity); 3],
}

/// Every button owned by the WORK tab (cells, name headers, the top-bar
/// toggle, close and defaults) — one query drives styling and clicks.
#[derive(Component, Clone, Copy)]
pub enum WorkTabButton {
    /// The top-bar "Work [Tab]" toggle (spawned by `ui::build_hud`).
    Toggle,
    Close,
    Reset,
    Name(usize),
    Cell(usize, WorkKind),
}

pub fn build_work_tab(mut commands: Commands) {
    let mut cols: Vec<WorkTabCol> = Vec::new();

    commands
        .spawn((
            WorkTabRoot,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(76.0),
                left: Val::Px(0.0),
                width: Val::Percent(100.0),
                align_items: AlignItems::Center,
                ..default()
            },
            Visibility::Hidden,
        ))
        .with_children(|root| {
            root.spawn((
                Interaction::default(),
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(4.0),
                    padding: UiRect::all(Val::Px(8.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BorderColor(Color::srgba(0.45, 0.55, 0.65, 0.8)),
                BackgroundColor(Color::srgba(0.07, 0.09, 0.12, 0.96)),
            ))
            .with_children(|panel| {
                // Title row: caption + defaults + close.
                panel
                    .spawn(Node {
                        flex_direction: FlexDirection::Row,
                        column_gap: Val::Px(6.0),
                        align_items: AlignItems::Center,
                        ..default()
                    })
                    .with_children(|row| {
                        label(row, "WORK", 15.0, Color::srgb(0.95, 0.85, 0.55));
                        label(
                            row,
                            "— who does what (click a cell to cycle)",
                            11.0,
                            Color::srgb(0.55, 0.6, 0.66),
                        );
                        row.spawn(Node {
                            flex_grow: 1.0,
                            ..default()
                        });
                        for (mark, text, action) in [
                            (WorkTabButton::Reset, "Defaults", Action::ResetWorkPriorities),
                            (WorkTabButton::Close, "Close [Tab]", Action::ToggleWorkTab),
                        ] {
                            row.spawn((
                                Button,
                                Interaction::default(),
                                OnPress(action),
                                mark,
                                Node {
                                    height: Val::Px(BTN_H),
                                    padding: UiRect::horizontal(Val::Px(10.0)),
                                    margin: UiRect::all(Val::Px(2.0)),
                                    align_items: AlignItems::Center,
                                    justify_content: JustifyContent::Center,
                                    ..default()
                                },
                                BackgroundColor(PANEL_BG),
                            ))
                            .with_children(|b| {
                                label(b, text, 12.0, Color::WHITE);
                            });
                        }
                    });

                // Header row: crew names (click = select the crew).
                panel
                    .spawn(Node {
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::FlexEnd,
                        ..default()
                    })
                    .with_children(|row| {
                        row.spawn(Node {
                            width: Val::Px(CORNER_W),
                            flex_direction: FlexDirection::Column,
                            ..default()
                        })
                        .with_children(|c| {
                            label(c, "crew", 12.0, Color::srgb(0.55, 0.62, 0.7));
                            label(
                                c,
                                "click a name to select",
                                9.0,
                                Color::srgb(0.42, 0.47, 0.53),
                            );
                        });
                        for i in 0..MAX_CREW_COLS {
                            let idx = cols.len();
                            let name_btn = row
                                .spawn((
                                    Button,
                                    Interaction::default(),
                                    WorkTabButton::Name(i),
                                    Node {
                                        width: Val::Px(COL_W),
                                        height: Val::Px(BTN_H),
                                        margin: UiRect::all(Val::Px(2.0)),
                                        align_items: AlignItems::Center,
                                        justify_content: JustifyContent::Center,
                                        ..default()
                                    },
                                    BackgroundColor(PANEL_BG),
                                ))
                                .with_children(|b| {
                                    cols.push(WorkTabCol {
                                        name_btn: Entity::PLACEHOLDER,
                                        name_label: label(b, "", 12.0, Color::WHITE),
                                        cur: Entity::PLACEHOLDER,
                                        stats: Entity::PLACEHOLDER,
                                        cells: [(Entity::PLACEHOLDER, Entity::PLACEHOLDER); 3],
                                    });
                                })
                                .id();
                            cols[idx].name_btn = name_btn;
                        }
                    });

                // Current-activity row.
                panel
                    .spawn(Node {
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::FlexStart,
                        ..default()
                    })
                    .with_children(|row| {
                        row.spawn(Node {
                            width: Val::Px(CORNER_W),
                            ..default()
                        })
                        .with_children(|c| {
                            label(c, "Current", 12.0, Color::srgb(0.55, 0.62, 0.7));
                        });
                        for col in cols.iter_mut() {
                            col.cur = fixed_label(row, "", 10.0, Color::srgb(0.65, 0.95, 0.7));
                        }
                    });

                // One row per work type.
                for kind in WorkKind::ALL {
                    panel
                        .spawn(Node {
                            flex_direction: FlexDirection::Row,
                            align_items: AlignItems::Center,
                            ..default()
                        })
                        .with_children(|row| {
                            row.spawn(Node {
                                width: Val::Px(CORNER_W),
                                flex_direction: FlexDirection::Column,
                                ..default()
                            })
                            .with_children(|c| {
                                label(c, kind.label(), 13.0, Color::WHITE);
                                label(c, kind.desc(), 9.0, Color::srgb(0.42, 0.47, 0.53));
                            });
                            let ki = work_kind_index(kind);
                            for (i, col) in cols.iter_mut().enumerate() {
                                let cell = row
                                    .spawn((
                                        Button,
                                        Interaction::default(),
                                        WorkTabButton::Cell(i, kind),
                                        Node {
                                            width: Val::Px(COL_W),
                                            height: Val::Px(BTN_H),
                                            margin: UiRect::all(Val::Px(2.0)),
                                            align_items: AlignItems::Center,
                                            justify_content: JustifyContent::Center,
                                            ..default()
                                        },
                                        BackgroundColor(Priority::Normal.bg()),
                                    ))
                                    .with_children(|b| {
                                        col.cells[ki].1 =
                                            label(b, "N", 13.0, Priority::Normal.color());
                                    })
                                    .id();
                                col.cells[ki].0 = cell;
                            }
                        });
                }

                // Lifetime counts row.
                panel
                    .spawn(Node {
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::FlexStart,
                        ..default()
                    })
                    .with_children(|row| {
                        row.spawn(Node {
                            width: Val::Px(CORNER_W),
                            ..default()
                        })
                        .with_children(|c| {
                            label(
                                c,
                                "Done (haul/build/operate)",
                                10.0,
                                Color::srgb(0.55, 0.62, 0.7),
                            );
                        });
                        for col in cols.iter_mut() {
                            col.stats = fixed_label(row, "", 10.0, Color::srgb(0.6, 0.66, 0.72));
                        }
                    });

                label(
                    panel,
                    "H high · N normal · L low · — off | priorities steer the NEXT job; running jobs finish first",
                    10.0,
                    Color::srgb(0.5, 0.55, 0.62),
                );
            });
        });

    commands.insert_resource(WorkTabHud { cols });
}

fn work_kind_index(kind: WorkKind) -> usize {
    WorkKind::ALL.iter().position(|k| *k == kind).unwrap()
}

/// Text with a fixed column width (keeps matrix columns aligned).
fn fixed_label(parent: &mut ChildSpawnerCommands, text: &str, size: f32, color: Color) -> Entity {
    parent
        .spawn((
            Text::new(text),
            TextFont {
                font_size: size,
                ..default()
            },
            TextColor(color),
            Node {
                width: Val::Px(COL_W),
                margin: UiRect::all(Val::Px(2.0)),
                ..default()
            },
        ))
        .id()
}

/// Consume `ToggleWorkTab` actions and drive the root visibility. Runs
/// separately from `work_tab_system` so the visibility write has no query
/// conflicts with the text/button pools.
pub(crate) fn work_tab_toggle_system(
    mut events: EventReader<Action>,
    mut visible: ResMut<WorkTabVisible>,
    mut vis_q: Query<&mut Visibility, With<WorkTabRoot>>,
    mut inited: Local<bool>,
) {
    if !*inited {
        *inited = true;
        // Screenshot/testing hook: start with the tab open.
        if std::env::var("SLICE7_VIEW").as_deref() == Ok("work") {
            visible.0 = true;
        }
    }
    for action in events.read() {
        if matches!(*action, Action::ToggleWorkTab) {
            visible.0 = !visible.0;
        }
    }
    for mut v in vis_q.iter_mut() {
        let want = if visible.0 {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *v != want {
            *v = want;
        }
    }
}

/// Encode a crew's priorities for the rebuild signature.
fn prio_code(c: &Crew) -> [u8; 3] {
    let enc = |p: Priority| -> u8 {
        match p {
            Priority::Disabled => 0,
            Priority::Low => 1,
            Priority::Normal => 2,
            Priority::High => 3,
        }
    };
    [
        enc(c.priorities.haul),
        enc(c.priorities.build),
        enc(c.priorities.operate),
    ]
}

fn truncate(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        s
    } else {
        let mut t: String = s.chars().take(max - 1).collect();
        t.push('…');
        t
    }
}

/// Maintain the matrix: rewrite cell buttons when the roster or any priority
/// changes, refresh the activity/stats lines on a wall-clock cadence, style
/// every button and turn name presses into selections.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn work_tab_system(
    mut commands: Commands,
    visible: Res<WorkTabVisible>,
    hud: Res<WorkTabHud>,
    mut selection: ResMut<Selection>,
    time: Res<Time<Real>>,
    crews: Query<(Entity, &Crew, &CrewTask)>,
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
        Option<&crate::building::Building>,
        Option<&crate::building::MarkedForDeconstruct>,
    )>,
    mut texts: Query<(&mut Text, &mut TextColor, &mut Visibility), Without<Button>>,
    mut btns: Query<
        (
            &WorkTabButton,
            &Interaction,
            &mut BackgroundColor,
            &mut Visibility,
        ),
        With<Button>,
    >,
    mut ui_acc: Local<f32>,
    mut sig: Local<Vec<(Entity, [u8; 3])>>,
) {
    if !visible.0 {
        return;
    }
    let roster: Vec<(Entity, &Crew, &CrewTask)> = crews.iter().collect();
    let want_sig: Vec<(Entity, [u8; 3])> =
        roster.iter().map(|(e, c, _)| (*e, prio_code(c))).collect();

    // ---- rebuild pass (roster, any tier, or a reopen changed) ----
    // Cells show the CURRENT tier; their click target carries the NEXT one.
    if *sig != want_sig || visible.is_changed() {
        *sig = want_sig;
        for (i, col) in hud.cols.iter().enumerate() {
            let shown = i < roster.len();
            if let Some((e, crew, _)) = roster.get(i) {
                for (ki, kind) in WorkKind::ALL.iter().enumerate() {
                    let cur = crew.priorities.get(*kind);
                    let (cell, cell_label) = col.cells[ki];
                    commands.entity(cell).insert(OnPress(Action::SetPriority {
                        crew: *e,
                        work: *kind,
                        level: cur.cycle(),
                    }));
                    if let Ok((mut text, mut color, _)) = texts.get_mut(cell_label) {
                        if text.0 != cur.code() {
                            text.0 = cur.code().to_string();
                        }
                        if color.0 != cur.color() {
                            color.0 = cur.color();
                        }
                    }
                }
                if let Ok((mut text, mut color, _)) = texts.get_mut(col.name_label) {
                    if text.0 != crew.name {
                        text.0 = crew.name.clone();
                    }
                    if color.0 != crew.tint {
                        color.0 = crew.tint;
                    }
                }
            }
            for e in [col.cur, col.stats] {
                if let Ok((_, _, mut v)) = texts.get_mut(e) {
                    *v = if shown {
                        Visibility::Visible
                    } else {
                        Visibility::Hidden
                    };
                }
            }
        }
    }

    // ---- per-frame styling + click-to-select + column visibility ----
    let sel_crew = match selection.0 {
        Some(Selected::Crew(e)) => Some(e),
        _ => None,
    };
    for (wbtn, interaction, mut bg, mut vis) in btns.iter_mut() {
        let shown = match wbtn {
            WorkTabButton::Toggle | WorkTabButton::Close | WorkTabButton::Reset => true,
            WorkTabButton::Name(i) | WorkTabButton::Cell(i, _) => *i < roster.len(),
        };
        let want_vis = if shown {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *vis != want_vis {
            *vis = want_vis;
        }
        let hovered = *interaction == Interaction::Hovered || *interaction == Interaction::Pressed;
        let want = match wbtn {
            WorkTabButton::Toggle => {
                if hovered {
                    BUTTON_HOVER
                } else if visible.0 {
                    BUTTON_ACTIVE
                } else {
                    BUTTON_BG
                }
            }
            WorkTabButton::Close | WorkTabButton::Reset => {
                if hovered {
                    BUTTON_HOVER
                } else {
                    PANEL_BG
                }
            }
            WorkTabButton::Name(i) => {
                if let Some((e, _, _)) = roster.get(*i) {
                    if Some(*e) == sel_crew {
                        Color::srgba(0.2, 0.3, 0.24, 1.0)
                    } else if hovered {
                        BUTTON_HOVER
                    } else {
                        PANEL_BG
                    }
                } else {
                    PANEL_BG
                }
            }
            WorkTabButton::Cell(i, kind) => {
                let base = roster
                    .get(*i)
                    .map(|(_, c, _)| c.priorities.get(*kind).bg())
                    .unwrap_or_else(|| Priority::Normal.bg());
                if hovered {
                    base.mix(&Color::WHITE, 0.18)
                } else {
                    base
                }
            }
        };
        if bg.0 != want {
            bg.0 = want;
        }
        // Name press = select the crew (handled here, not via OnPress).
        if let WorkTabButton::Name(i) = wbtn {
            if *interaction == Interaction::Pressed && roster.get(*i).is_some() {
                selection.0 = Some(Selected::Crew(roster[*i].0));
            }
        }
    }

    // ---- activity + stats lines on a wall-clock cadence ----
    *ui_acc += time.delta_secs();
    if *ui_acc < 0.1 {
        return;
    }
    *ui_acc = 0.0;
    for (i, col) in hud.cols.iter().enumerate() {
        let Some((_, crew, task)) = roster.get(i) else {
            continue;
        };
        if let Ok((mut text, mut color, _)) = texts.get_mut(col.cur) {
            let idle = matches!(task, CrewTask::Idle(_));
            let want = truncate(crate::ui::task_label(task, &items, &racks), 26);
            if text.0 != want {
                text.0 = want;
            }
            let want_c = if idle {
                Color::srgb(0.55, 0.58, 0.64)
            } else {
                Color::srgb(0.65, 0.95, 0.7)
            };
            if color.0 != want_c {
                color.0 = want_c;
            }
        }
        if let Ok((mut text, _, _)) = texts.get_mut(col.stats) {
            let want = format!("{}/{}/{}", crew.delivered, crew.built, crew.operated);
            if text.0 != want {
                text.0 = want;
            }
        }
    }
}
