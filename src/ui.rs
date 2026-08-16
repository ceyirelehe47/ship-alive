//! HUD, laid out RimWorld-style as floating corner panels over the map:
//! top-left identity/stats and the event feed, top-right the ship clock with
//! time controls and alerts, bottom-left the build bar (categories open a
//! flyout above it), bottom-center per-crew status chips, and a bottom-right
//! inspect pane (ship status, or selection details with buttons for
//! build/deconstruct, rack filters, fabricator orders and crew work
//! priorities).
//!
//! Every text is rebuilt each frame — at slice scale this is negligible and
//! keeps the update code trivial. All labels are ASCII because the bundled
//! default font has no CJK glyphs (recorded as a temporary behavior).
//!
//! The selection panel's *buttons* are fixed slots that get re-purposed
//! (label + OnPress action + visibility) whenever the selection or its
//! discrete state changes, so interactions never break on rebuilds.

use crate::building::{Building, BuildingKind, MarkedForDeconstruct};
use crate::coolant::{CoolantState, WaterGrid};
use crate::crew::{Crew, CrewTask, HaulPhase, Priority, WorkKind};
use crate::input::{BuildMode, Selected, Selection, Tool};
use crate::items::{CarriedBy, Item, ItemKind, MarkedForHaul, NoPathUntil, ReservedBy};
use crate::jobs::Action;
use crate::log::{EventLog, LogKind};
use crate::map::TilePos;
use crate::power::{PowerRole, PowerState, PowerStatus};
use crate::storage::StorageCell;
use crate::thermal::ThermalGrid;
use crate::time_ctrl::GameSpeed;
use crate::OverlayMode;
use bevy::ecs::system::SystemParam;
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

/// Build toolbar categories. The bar shows only these; clicking one opens a
/// flyout listing its concrete buildings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BuildCatKind {
    Structure,
    Storage,
    Machines,
    Power,
    Thermal,
}

impl BuildCatKind {
    pub const ALL: [BuildCatKind; 5] = [
        BuildCatKind::Structure,
        BuildCatKind::Storage,
        BuildCatKind::Machines,
        BuildCatKind::Power,
        BuildCatKind::Thermal,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BuildCatKind::Structure => "Structure",
            BuildCatKind::Storage => "Storage",
            BuildCatKind::Machines => "Machines",
            BuildCatKind::Power => "Power",
            BuildCatKind::Thermal => "Thermal",
        }
    }

    pub fn kinds(self) -> &'static [BuildingKind] {
        match self {
            BuildCatKind::Structure => &[BuildingKind::Wall, BuildingKind::Door],
            BuildCatKind::Storage => &[BuildingKind::Rack],
            BuildCatKind::Machines => &[BuildingKind::Fabricator, BuildingKind::Reactor],
            BuildCatKind::Power => &[BuildingKind::PowerCable],
            BuildCatKind::Thermal => &[
                BuildingKind::CoolantPipe,
                BuildingKind::Pump,
                BuildingKind::HeatExchanger,
                BuildingKind::Radiator,
                BuildingKind::Reservoir,
            ],
        }
    }
}

/// Which build category's flyout is currently open.
#[derive(Resource, Default, Debug)]
pub struct BuildMenu(pub Option<BuildCatKind>);

/// A category button in the build bar.
#[derive(Component)]
pub struct BuildCat(pub BuildCatKind);

/// One flyout row (the buildings of one category).
#[derive(Component)]
pub struct FlyoutRow(pub BuildCatKind);

/// The flyout container above the build bar.
#[derive(Component)]
pub struct FlyoutRoot;

/// Marks which tool a build-bar button represents (for highlight state).
#[derive(Component)]
pub struct ToolIndex(pub Tool);

/// The collapsed-by-default developer toolbar visibility flag.
#[derive(Resource, Default)]
pub struct DebugBarVisible(pub bool);

/// `Visibility::Hidden` is render-only in Bevy UI — the node still occupies
/// layout space. Panels that must physically collapse (flyout, inspect
/// sections, debug row, unused button slots) carry this marker and have
/// their `Display` mirrored from `Visibility` by `collapse_hidden_system`.
#[derive(Component)]
pub struct CollapseWhenHidden;

/// Map `Visibility` onto the `Node`'s `display` field for marked nodes so
/// hidden panels stop reserving space in the flexbox layout.
fn collapse_hidden_system(mut q: Query<(&Visibility, &mut Node), With<CollapseWhenHidden>>) {
    for (vis, mut node) in &mut q {
        let want = if *vis == Visibility::Hidden {
            Display::None
        } else {
            Display::Flex
        };
        if node.display != want {
            node.display = want;
        }
    }
}

/// Number of fixed text lines / button slots in the selection panel.
const SEL_LINES: usize = 12;
const SEL_BTNS: usize = 16;
/// Fixed text lines in the inspect pane's environment view (room for the
/// full power / thermal / compartments / storage / production blocks).
const ENV_LINES: usize = 32;

#[derive(Resource)]
pub struct Hud {
    pub speed_buttons: Vec<Entity>,
    pub stats: Entity,
    pub ship_time: Entity,
    pub chips: Vec<Entity>,
    pub sel_lines: Vec<Entity>,
    pub sel_btn_row1: Entity,
    pub sel_btn_row2: Entity,
    pub sel_btns: Vec<Entity>,
    pub log_lines: Vec<Entity>,
    pub debug_row: Entity,
    pub debug_button_label: Entity,
    pub sim_telemetry: Entity,
    pub tool_hint: Entity,
    pub tool_buttons: Vec<Entity>,
    pub power_button_label: Entity,
    pub power_line: Entity,
    pub alert_line: Entity,
    pub build_cat_buttons: Vec<(BuildCatKind, Entity)>,
    pub flyout: Entity,
    pub flyout_rows: Vec<(BuildCatKind, Entity)>,
    pub env_lines: Vec<Entity>,
    pub env_section: Entity,
    pub entity_section: Entity,
}

/// Wall-clock UI refresh cadence for sim-derived text (sidebar, HUD lines,
/// overlay summaries). Fast enough to read as live, slow enough that text
/// layout does not run every frame.
const UI_REFRESH_SECS: f32 = 0.2;

/// Assign UI text only when it actually changed — avoids re-triggering
/// Bevy's text layout for mostly-static lines.
fn set_text_if_changed(text: &mut Text, want: String) {
    if text.0 != want {
        text.0 = want;
    }
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

/// Read-only world view bundled into one system param (keeps the selection
/// panel and sidebars under the 16-parameter limit). Thermal + coolant +
/// airtight + atmosphere state, including the live door query.
#[derive(SystemParam)]
pub struct ThermalView<'w, 's> {
    pub grid: Res<'w, ThermalGrid>,
    pub coolant: Res<'w, CoolantState>,
    pub water: Res<'w, WaterGrid>,
    pub comps: Res<'w, crate::airtight::Compartments>,
    pub doors: Query<'w, 's, &'static crate::airtight::Door>,
    pub atmo: Res<'w, crate::atmosphere::AtmosphereGrid>,
    pub atmo_summary: Res<'w, crate::atmosphere::AtmoSummary>,
    pub atmo_stats: Res<'w, crate::atmosphere::AtmoStats>,
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
        app.init_resource::<BuildMenu>();
        app.add_systems(Startup, (build_hud, crate::ui_overlay::build_overlay));
        app.add_systems(
            Update,
            (
                button_system,
                btn_label_system,
                sidebar_system,
                build_menu_system,
                overlay_cycle_system,
                overlay_summary_system,
                debug_toggle_system,
                (hud_update_system, selection_panel_system).chain(),
                (
                    crate::ui_overlay::tooltip_system,
                    crate::ui_overlay::atmosphere_tooltip_system,
                )
                    .chain(),
                crate::ui_overlay::box_rect_system,
            )
                .in_set(crate::Set::Sync),
        );
        // Must run after the Sync systems that write Visibility so panels
        // collapse in the same frame they hide (layout runs in PostUpdate).
        app.add_systems(Update, collapse_hidden_system.after(crate::Set::Sync));
    }
}

fn build_hud(mut commands: Commands) {
    let mut speed_buttons = Vec::new();
    let mut stats = Entity::PLACEHOLDER;
    let mut ship_time_label = Entity::PLACEHOLDER;
    let mut alert_line = Entity::PLACEHOLDER;
    let mut chips = Vec::new();
    let mut sel_lines = Vec::new();
    let mut sel_btns = Vec::new();
    let mut log_lines = Vec::new();
    let mut debug_row = Entity::PLACEHOLDER;
    let mut debug_button_label = Entity::PLACEHOLDER;
    let mut sim_telemetry = Entity::PLACEHOLDER;
    let mut tool_hint = Entity::PLACEHOLDER;
    let mut tool_buttons = Vec::new();
    let mut power_button_label = Entity::PLACEHOLDER;
    let mut power_line = Entity::PLACEHOLDER;
    let mut build_cat_buttons: Vec<(BuildCatKind, Entity)> = Vec::new();
    let mut flyout = Entity::PLACEHOLDER;
    let mut flyout_rows: Vec<(BuildCatKind, Entity)> = Vec::new();
    let mut pending_tool_btns: Vec<(Entity, Tool)> = Vec::new();
    let mut env_lines: Vec<Entity> = Vec::new();
    let mut env_section = Entity::PLACEHOLDER;
    let mut entity_section = Entity::PLACEHOLDER;

    commands
        .spawn(Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            ..default()
        })
        .with_children(|root| {
            // ---- top: left cluster (identity, stats, event feed) and right
            // cluster (ship clock, time controls, alerts) ----
            root.spawn(Node {
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::FlexStart,
                ..default()
            })
            .with_children(|top| {
                // Top-left panel.
                top.spawn((
                    Interaction::default(),
                    Node {
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(2.0),
                        padding: UiRect::all(Val::Px(6.0)),
                        ..default()
                    },
                    BackgroundColor(PANEL_BG),
                ))
                .with_children(|panel| {
                    panel.spawn(Node {
                        flex_direction: FlexDirection::Row,
                        column_gap: Val::Px(6.0),
                        align_items: AlignItems::Center,
                        flex_wrap: FlexWrap::Wrap,
                        ..default()
                    })
                    .with_children(|row| {
                        label(row, "SHIP ALIVE", 15.0, Color::srgb(0.95, 0.85, 0.55));
                        stats = label(row, "", 14.0, Color::WHITE);
                        button(row, "Haul All [H]", Action::MarkAll, 96.0);
                        button(row, "Cancel All [C]", Action::CancelAll, 104.0);
                    });

                    // Per-network power summary line (visible with the overlay).
                    power_line = label(panel, "", 12.0, Color::srgb(0.62, 0.9, 0.8));

                    // Event feed.
                    panel.spawn((
                        Interaction::default(),
                        Node {
                            padding: UiRect::all(Val::Px(4.0)),
                            flex_direction: FlexDirection::Column,
                            row_gap: Val::Px(1.0),
                            ..default()
                        },
                    ))
                    .with_children(|p| {
                        label(p, "EVENT LOG", 11.0, Color::srgb(0.5, 0.55, 0.62));
                        for _ in 0..EventLog::VISIBLE {
                            log_lines.push(label(p, "", 12.0, Color::srgb(0.75, 0.78, 0.82)));
                        }
                    });

                    // Developer toolbar, hidden by default.
                    debug_row = panel
                        .spawn((
                            Node {
                                flex_direction: FlexDirection::Row,
                                column_gap: Val::Px(6.0),
                                align_items: AlignItems::Center,
                                ..default()
                            },
                            CollapseWhenHidden,
                            Visibility::Hidden,
                        ))
                        .with_children(|row| {
                            button(
                                row,
                                "+Crate",
                                Action::SpawnItem { kind: ItemKind::Crate },
                                64.0,
                            );
                            button(row, "+Ore", Action::SpawnItem { kind: ItemKind::Ore }, 56.0);
                            button(row, "+Part", Action::SpawnItem { kind: ItemKind::Part }, 60.0);
                            label(
                                row,
                                "debug tools | [X] deletes the selected item",
                                11.0,
                                Color::srgb(0.55, 0.6, 0.66),
                            );
                            sim_telemetry = label(row, "", 11.0, Color::srgb(0.6, 0.8, 0.7));
                        })
                        .id();

                    label(
                        panel,
                        "Drag: mark items for hauling | Click: select | Right-drag / WASD: pan | Wheel: zoom | T: mark | B: build tools | Space/1/2/3: speed",
                        11.0,
                        Color::srgb(0.6, 0.66, 0.72),
                    );
                });

                // Top-right panel: ship clock, time controls and alerts.
                top.spawn((
                    Interaction::default(),
                    Node {
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::FlexEnd,
                        row_gap: Val::Px(2.0),
                        padding: UiRect::all(Val::Px(6.0)),
                        flex_shrink: 0.0,
                        ..default()
                    },
                    BackgroundColor(PANEL_BG),
                ))
                .with_children(|panel| {
                    panel.spawn(Node {
                        flex_direction: FlexDirection::Row,
                        column_gap: Val::Px(4.0),
                        align_items: AlignItems::Center,
                        ..default()
                    })
                    .with_children(|row| {
                        ship_time_label = label(row, "", 14.0, Color::srgb(0.62, 0.9, 0.8));
                        for (i, text) in ["Pause", "1x", "2x", "4x"].iter().enumerate() {
                            speed_buttons
                                .push(button(row, text, Action::SetSpeed { index: i }, 52.0));
                        }
                        row.spawn((
                            Button,
                            Interaction::default(),
                            OnPress(Action::CycleOverlay),
                            Node {
                                width: Val::Px(96.0),
                                height: Val::Px(26.0),
                                margin: UiRect::all(Val::Px(2.0)),
                                align_items: AlignItems::Center,
                                justify_content: JustifyContent::Center,
                                ..default()
                            },
                            BackgroundColor(BUTTON_BG),
                        ))
                        .with_children(|b| {
                            power_button_label = label(b, "View [P]", 13.0, Color::WHITE);
                        });
                        row.spawn((
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
                        });
                    });

                    alert_line = label(panel, "", 13.0, Color::srgb(1.0, 0.4, 0.3));
                });
            });

            // ---- bottom: build bar (left), crew chips (center), inspect
            // pane (right) ----
            // The row grows to fill the leftover height so its FlexEnd
            // alignment pins the panels to the true screen bottom; the fixed
            // panels must not shrink and the chips take only leftover width
            // (flex-basis 0), otherwise the row overflows and crushes them.
            root.spawn(Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::FlexEnd,
                column_gap: Val::Px(6.0),
                flex_grow: 1.0,
                ..default()
            })
            .with_children(|bottom| {
                // Build panel: the category bar sits in the bottom-left
                // corner; its flyout and the placement hint open above it.
                bottom.spawn((
                    Interaction::default(),
                    Node {
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::FlexStart,
                        row_gap: Val::Px(2.0),
                        flex_shrink: 0.0,
                        ..default()
                    },
                ))
                .with_children(|panel| {
                    tool_hint = label(panel, "", 12.0, Color::srgb(0.6, 0.8, 0.65));

                    // Flyout: the concrete buildings of the opened category.
                    flyout = panel
                        .spawn((
                            FlyoutRoot,
                            Interaction::default(),
                            CollapseWhenHidden,
                            Node {
                                flex_direction: FlexDirection::Column,
                                padding: UiRect::all(Val::Px(6.0)),
                                margin: UiRect::all(Val::Px(2.0)),
                                border: UiRect::all(Val::Px(1.0)),
                                ..default()
                            },
                            BorderColor(Color::srgba(0.45, 0.55, 0.65, 0.8)),
                            BackgroundColor(Color::srgba(0.09, 0.12, 0.16, 0.95)),
                            Visibility::Hidden,
                        ))
                        .with_children(|f| {
                            for cat in BuildCatKind::ALL {
                                let row_e = f
                                    .spawn((
                                        FlyoutRow(cat),
                                        CollapseWhenHidden,
                                        Node {
                                            flex_direction: FlexDirection::Row,
                                            column_gap: Val::Px(4.0),
                                            align_items: AlignItems::Center,
                                            ..default()
                                        },
                                        Visibility::Hidden,
                                    ))
                                    .with_children(|r| {
                                        let header = match cat {
                                            BuildCatKind::Structure => "Structure:",
                                            BuildCatKind::Storage => "Storage:",
                                            BuildCatKind::Machines => "Machines:",
                                            BuildCatKind::Power => "Power:",
                                            BuildCatKind::Thermal => "Thermal:",
                                        };
                                        label(r, header, 11.0, Color::srgb(0.55, 0.62, 0.7));
                                        for kind in cat.kinds() {
                                            let tool = Tool::Build(*kind);
                                            let e = button(
                                                r,
                                                kind.label(),
                                                Action::SetTool { tool: Some(tool) },
                                                match kind {
                                                    BuildingKind::Fabricator => 88.0,
                                                    BuildingKind::Door => 56.0,
                                                    BuildingKind::Wall => 56.0,
                                                    BuildingKind::Rack => 96.0,
                                                    BuildingKind::PowerCable => 96.0,
                                                    BuildingKind::Reactor => 72.0,
                                                    BuildingKind::CoolantPipe => 96.0,
                                                    BuildingKind::Pump => 96.0,
                                                    BuildingKind::Reservoir => 110.0,
                                                    BuildingKind::HeatExchanger => 118.0,
                                                    BuildingKind::Radiator => 76.0,
                                                },
                                            );
                                            pending_tool_btns.push((e, tool));
                                            tool_buttons.push(e);
                                        }
                                    })
                                    .id();
                                flyout_rows.push((cat, row_e));
                            }
                        })
                        .id();

                    // Category bar (RimWorld-style architect tabs). Only
                    // categories live here; their buildings sit in the
                    // flyout opened on click (see build_menu_system).
                    panel.spawn((
                        Interaction::default(),
                        Node {
                            flex_direction: FlexDirection::Row,
                            column_gap: Val::Px(4.0),
                            align_items: AlignItems::Center,
                            padding: UiRect::vertical(Val::Px(4.0)),
                            ..default()
                        },
                        BackgroundColor(PANEL_BG),
                    ))
                    .with_children(|row| {
                        for cat in BuildCatKind::ALL {
                            let e = row
                                .spawn((
                                    Button,
                                    Interaction::default(),
                                    BuildCat(cat),
                                    Node {
                                        height: Val::Px(26.0),
                                        padding: UiRect::horizontal(Val::Px(10.0)),
                                        margin: UiRect::all(Val::Px(2.0)),
                                        align_items: AlignItems::Center,
                                        justify_content: JustifyContent::Center,
                                        ..default()
                                    },
                                    BackgroundColor(BUTTON_BG),
                                ))
                                .with_children(|b| {
                                    label(b, cat.label(), 13.0, Color::WHITE);
                                })
                                .id();
                            build_cat_buttons.push((cat, e));
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
                    });
                });

                // Crew chips (bottom-center): basis 0 so they only ever take
                // the space the corner panels leave, wrapping as needed.
                bottom.spawn((
                    Interaction::default(),
                    Node {
                        flex_direction: FlexDirection::Row,
                        column_gap: Val::Px(6.0),
                        row_gap: Val::Px(4.0),
                        flex_wrap: FlexWrap::Wrap,
                        flex_grow: 1.0,
                        flex_basis: Val::Px(0.0),
                        justify_content: JustifyContent::Center,
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

                // Inspect pane: environment overview by default, selection
                // details + operation buttons while something is selected.
                bottom.spawn((
                    Interaction::default(),
                    Node {
                        width: Val::Px(300.0),
                        padding: UiRect::all(Val::Px(8.0)),
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(2.0),
                        flex_shrink: 0.0,
                        ..default()
                    },
                    BackgroundColor(PANEL_BG),
                ))
                .with_children(|sb| {
                    label(sb, "SHIP STATUS", 12.0, Color::srgb(0.5, 0.55, 0.62));

                    // Environment mode (nothing selected).
                    env_section = sb
                        .spawn((
                            CollapseWhenHidden,
                            Node {
                                flex_direction: FlexDirection::Column,
                                row_gap: Val::Px(2.0),
                                ..default()
                            },
                        ))
                        .with_children(|sec| {
                            for _ in 0..ENV_LINES {
                                env_lines.push(label(sec, "", 12.0, Color::WHITE));
                            }
                        })
                        .id();

                    // Entity mode (something selected): properties + operations.
                    entity_section = sb
                        .spawn((
                            CollapseWhenHidden,
                            Node {
                                flex_direction: FlexDirection::Column,
                                row_gap: Val::Px(2.0),
                                ..default()
                            },
                            Visibility::Hidden,
                        ))
                        .with_children(|sec| {
                            for _ in 0..SEL_LINES {
                                sel_lines.push(label(sec, "", 13.0, Color::WHITE));
                            }
                            // Re-purposable operation buttons (wrap to pane width).
                            for _row_idx in 0..2 {
                                sec.spawn(Node {
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
                                                CollapseWhenHidden,
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
                        })
                        .id();
                });
            });
        });
    commands.insert_resource(Hud {
        speed_buttons: speed_buttons.clone(),
        stats,
        ship_time: ship_time_label,
        chips,
        sel_lines,
        sel_btn_row1: Entity::PLACEHOLDER,
        sel_btn_row2: Entity::PLACEHOLDER,
        sel_btns,
        log_lines,
        debug_row,
        debug_button_label,
        sim_telemetry,
        tool_hint,
        tool_buttons,
        power_button_label,
        power_line,
        alert_line,
        build_cat_buttons,
        flyout,
        flyout_rows,
        env_lines,
        env_section,
        entity_section,
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

/// Inspect pane (bottom-right): environment overview by default, selection
/// details + operation buttons while something is selected.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn sidebar_system(
    hud: Res<Hud>,
    // Real time: the UI refresh cadence is wall-clock (the virtual clock
    // runs at BASE_SIM_RATE × game speed and pauses with the game).
    time: Res<Time<Real>>,
    mut ui_acc: Local<f32>,
    selection: Res<Selection>,
    clock: Res<crate::simtime::SimClock>,
    speed: Res<GameSpeed>,
    stats: Res<crate::stats::Stats>,
    power_state: Res<PowerState>,
    thermal: ThermalView,
    reactors: Query<(&crate::building::Footprint, &crate::thermal::ThermalState)>,
    racks: Query<&StorageCell>,
    items: Query<(&Item, Option<&MarkedForHaul>), With<Item>>,
    fabs: Query<&PowerStatus, With<crate::production::Fabricator>>,
    crews: Query<&CrewTask, With<Crew>>,
    mut texts: Query<(&mut Text, &mut TextColor, &mut Visibility), Without<Button>>,
    mut vis_q: Query<&mut Visibility, (With<Node>, Without<Text>, Without<Button>)>,
) {
    // ---- mode switch ----
    let entity_mode = selection.0.is_some();
    if let Ok(mut v) = vis_q.get_mut(hud.env_section) {
        *v = if entity_mode {
            Visibility::Hidden
        } else {
            Visibility::Visible
        };
    }
    if let Ok(mut v) = vis_q.get_mut(hud.entity_section) {
        *v = if entity_mode {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    if !entity_mode {
        // Environment content is sim-derived and refreshes on a cadence
        // (includes a full-grid hottest-room scan).
        *ui_acc += time.delta_secs();
        if *ui_acc < UI_REFRESH_SECS {
            return;
        }
        *ui_acc = 0.0;
        // ---- environment content ----
        let ship_time = crate::simtime::format_sim_stamp(clock.now());

        let stored: u32 = racks.iter().map(|c| c.stored()).sum();
        let cap: u32 = racks.iter().map(|c| c.capacity).sum();
        let mut rack_counts = [0u32; 3];
        for c in racks.iter() {
            for k in ItemKind::ALL {
                rack_counts[k.index()] += c.counts[k.index()];
            }
        }
        let mut ground = [0u32; 3];
        let mut marked = 0u32;
        for (it, m) in items.iter() {
            ground[it.kind.index()] += 1;
            if m.is_some() {
                marked += 1;
            }
        }
        let fabs_online = fabs.iter().filter(|p| p.ok()).count();
        let fabs_total = fabs.iter().count();
        let idle = crews
            .iter()
            .filter(|t| matches!(t, CrewTask::Idle(_)))
            .count();

        let dim = Color::srgb(0.55, 0.6, 0.66);
        let mut lines: Vec<(String, Color)> = vec![
            (
                format!("Time {ship_time} | {}", speed.label()),
                Color::WHITE,
            ),
            (String::new(), Color::WHITE),
            ("POWER".to_string(), dim),
        ];
        if power_state.networks.is_empty() {
            lines.push(("no networks (lay cables)".into(), dim));
        }
        for (i, net) in power_state.networks.iter().enumerate().take(3) {
            lines.push((format!("NET {}: {}", i + 1, net.summary()), {
                if net.generation == 0 || net.demand > net.generation {
                    Color::srgb(1.0, 0.6, 0.45)
                } else {
                    Color::srgb(0.7, 0.95, 0.75)
                }
            }));
        }
        // Thermal block: cores + hottest room + coolant loops.
        let mut hottest_core = f32::NEG_INFINITY;
        let mut worst_state = crate::thermal::ThermalState::Normal;
        for (foot, state) in reactors.iter() {
            let t = thermal.grid.max_footprint_temp(foot);
            hottest_core = hottest_core.max(t);
            worst_state = worst_state.max(*state);
        }
        let mut ship_max = f32::NEG_INFINITY;
        for &t in &thermal.grid.amb {
            ship_max = ship_max.max(t);
        }
        let total_water = thermal.water.total_water();
        lines.push((String::new(), Color::WHITE));
        lines.push(("THERMAL".to_string(), dim));
        if reactors.is_empty() {
            lines.push(("no reactor installed".into(), dim));
        } else {
            lines.push((
                format!("Core: {:.0}°C ({})", hottest_core, worst_state.label()),
                match worst_state {
                    crate::thermal::ThermalState::Normal => Color::srgb(0.7, 0.95, 0.75),
                    crate::thermal::ThermalState::Overheat => Color::srgb(1.0, 0.7, 0.25),
                    crate::thermal::ThermalState::Critical => Color::srgb(1.0, 0.4, 0.3),
                },
            ));
            lines.push((format!("Hottest room: {:.0}°C", ship_max), Color::WHITE));
        }
        if thermal.coolant.networks.is_empty() {
            lines.push(("Coolant: none (lay pipes)".into(), dim));
        } else {
            let dumping: f32 = thermal.coolant.networks.iter().map(|n| n.dump_rate).sum();
            lines.push((
                format!(
                    "Coolant: {} net | water {:.0} | dumping {:.0}H/s",
                    thermal.coolant.networks.len(),
                    total_water,
                    dumping
                ),
                Color::WHITE,
            ));
        }
        // Airtight compartments block (Slice 4).
        lines.push((String::new(), Color::WHITE));
        lines.push(("COMPARTMENTS".to_string(), dim));
        let comps = &thermal.comps;
        let exposed = comps.exposed_count();
        lines.push((
            format!(
                "{} structural | {} sealed | {} exposed",
                comps.regions.len(),
                comps.sealed_count(),
                exposed
            ),
            if exposed > 0 {
                Color::srgb(1.0, 0.45, 0.35)
            } else {
                Color::WHITE
            },
        ));
        let air_note = if comps.air_groups as usize == comps.regions.len() {
            String::new()
        } else {
            format!(" | air regions {}", comps.air_groups)
        };
        lines.push((
            format!(
                "Doors: {} closed / {} open{air_note}",
                comps.doors_closed, comps.doors_open
            ),
            Color::WHITE,
        ));
        // Atmosphere block (Slice 5) — reads the cached summary only, never
        // a per-frame grid scan.
        lines.push((String::new(), Color::WHITE));
        lines.push(("ATMOSPHERE".to_string(), dim));
        let a = &thermal.atmo_summary;
        let exposed = comps.exposed_count();
        lines.push((
            format!(
                "Pressure     {:.0}–{:.0} kPa",
                a.min_pressure, a.max_pressure
            ),
            if a.low_cells > 0 || a.vacuum_cells > 0 {
                Color::srgb(1.0, 0.45, 0.35)
            } else {
                Color::WHITE
            },
        ));
        lines.push((
            format!(
                "O2 partial   {:.1}–{:.1} kPa",
                a.min_o2_partial, a.max_o2_partial
            ),
            if a.low_o2_cells > 0 {
                Color::srgb(1.0, 0.6, 0.4)
            } else {
                Color::WHITE
            },
        ));
        lines.push((
            format!(
                "Gas retained {:.0}%{}",
                a.retained * 100.0,
                if a.max_co2_partial > crate::atmosphere::CO2_HIGH_KPA {
                    format!(" | CO2 {:.1} kPa", a.max_co2_partial)
                } else {
                    String::new()
                }
            ),
            if a.high_co2_cells > 0 || a.polluted_cells > 0 {
                Color::srgb(1.0, 0.7, 0.35)
            } else {
                Color::WHITE
            },
        ));
        lines.push((
            format!(
                "Exposed      {} compartments{}",
                exposed,
                if a.active_cells > 0 {
                    format!(" | venting, {} cells active", a.active_cells)
                } else {
                    String::new()
                }
            ),
            if exposed > 0 {
                Color::srgb(1.0, 0.45, 0.35)
            } else {
                Color::WHITE
            },
        ));
        lines.push((String::new(), Color::WHITE));
        lines.push(("STORAGE".to_string(), dim));
        lines.push((
            format!(
                "Stored {stored}/{cap}{}",
                if cap == stored { " FULL" } else { "" }
            ),
            if cap == stored {
                Color::srgb(1.0, 0.45, 0.4)
            } else {
                Color::WHITE
            },
        ));
        lines.push((
            format!(
                "Racks   Ore {} | Part {} | Crate {}",
                rack_counts[ItemKind::Ore.index()],
                rack_counts[ItemKind::Part.index()],
                rack_counts[ItemKind::Crate.index()]
            ),
            Color::WHITE,
        ));
        lines.push((
            format!(
                "Ground  Ore {} | Part {} | Crate {}",
                ground[ItemKind::Ore.index()],
                ground[ItemKind::Part.index()],
                ground[ItemKind::Crate.index()]
            ),
            Color::WHITE,
        ));
        lines.push((format!("Marked for haul: {marked}",), Color::WHITE));
        lines.push((String::new(), Color::WHITE));
        lines.push(("PRODUCTION".to_string(), dim));
        lines.push((
            format!("Parts made {} | Built {}", stats.produced, stats.built),
            Color::WHITE,
        ));
        lines.push((
            format!("Fabricators {fabs_online}/{fabs_total} powered"),
            if fabs_online < fabs_total {
                Color::srgb(1.0, 0.6, 0.45)
            } else {
                Color::WHITE
            },
        ));
        lines.push((
            format!(
                "Crew idle {}/{} | Hauled {}",
                idle,
                crews.iter().count(),
                stats.hauls_done
            ),
            Color::WHITE,
        ));
        for (i, line) in hud.env_lines.iter().enumerate() {
            if let Ok((mut text, mut color, mut vis)) = texts.get_mut(*line) {
                if i < lines.len() {
                    *vis = Visibility::Visible;
                    if text.0 != lines[i].0 {
                        text.0 = lines[i].0.clone();
                    }
                    if color.0 != lines[i].1 {
                        color.0 = lines[i].1;
                    }
                } else {
                    *vis = Visibility::Hidden;
                }
            }
        }
    } else {
        // Selection content is written by selection_panel_system.
        for line in hud.env_lines.iter() {
            if let Ok((_, _, mut vis)) = texts.get_mut(*line) {
                *vis = Visibility::Hidden;
            }
        }
    }
}

/// Build toolbar flyout: category clicks toggle it open/closed, picking a
/// building (or any tool change) closes it, and the open/active category
/// button is highlighted.
#[allow(clippy::type_complexity)]
fn build_menu_system(
    mut menu: ResMut<BuildMenu>,
    hud: Res<Hud>,
    build_mode: Res<crate::input::BuildMode>,
    cat_q: Query<(&Interaction, &BuildCat), Changed<Interaction>>,
    mut vis_q: Query<&mut Visibility, Or<(With<FlyoutRoot>, With<FlyoutRow>)>>,
    mut bg_q: Query<(&BuildCat, &Interaction, &mut BackgroundColor)>,
) {
    // Toggle on click (Changed+Pressed = only the transition frame, so
    // holding the button does not flicker the menu open and closed).
    for (interaction, cat) in cat_q.iter() {
        if *interaction == Interaction::Pressed {
            menu.0 = if menu.0 == Some(cat.0) {
                None
            } else {
                Some(cat.0)
            };
        }
    }
    // Any tool change (pick from the flyout, B cycle, Esc, Deconstruct…)
    // closes the flyout so the placement ghost takes over immediately.
    if build_mode.is_changed() {
        menu.0 = None;
    }

    let open = menu.0;
    if let Ok(mut v) = vis_q.get_mut(hud.flyout) {
        *v = if open.is_some() {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    for (cat, row_e) in &hud.flyout_rows {
        if let Ok(mut v) = vis_q.get_mut(*row_e) {
            *v = if open == Some(*cat) {
                Visibility::Visible
            } else {
                Visibility::Hidden
            };
        }
    }
    // Highlight: open category, or the category owning the active tool.
    let active_tool_cat = match build_mode.0 {
        Some(Tool::Build(kind)) => BuildCatKind::ALL
            .iter()
            .find(|c| c.kinds().contains(&kind))
            .copied(),
        _ => None,
    };
    for (cat_button, interaction, mut bg) in bg_q.iter_mut() {
        let cat = cat_button.0;
        bg.0 = if open == Some(cat) || active_tool_cat == Some(cat) {
            BUTTON_ACTIVE
        } else if *interaction == Interaction::Hovered {
            BUTTON_HOVER
        } else {
            BUTTON_BG
        };
    }
}

/// Cycle the map overlay view (button + P hotkey): off → power → thermal →
/// coolant → off. Modes are mutually exclusive by construction.
fn overlay_cycle_system(
    mut events: EventReader<Action>,
    mut overlay: ResMut<OverlayMode>,
    hud: Res<Hud>,
    mut log: ResMut<EventLog>,
    clock: Res<crate::simtime::SimClock>,
    mut text_q: Query<&mut Text>,
) {
    let mut cycled = false;
    for action in events.read() {
        if matches!(action, Action::CycleOverlay) {
            *overlay = overlay.cycle();
            cycled = true;
        }
    }
    if !cycled {
        return;
    }
    let now = clock.now();
    log.push(
        now,
        LogKind::Info,
        format!("Overlay view: {}", overlay.label()),
    );
    if let Ok(mut text) = text_q.get_mut(hud.power_button_label) {
        text.0 = format!("View: {} [P]", overlay.label());
    }
}

/// Overlay summary line under the bar + the always-on thermal alert.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn overlay_summary_system(
    hud: Res<Hud>,
    // Real time: the UI refresh cadence is wall-clock (the virtual clock
    // runs at BASE_SIM_RATE × game speed and pauses with the game).
    time: Res<Time<Real>>,
    mut ui_acc: Local<f32>,
    mut last_mode: Local<OverlayMode>,
    overlay: Res<OverlayMode>,
    power_state: Res<PowerState>,
    thermal: ThermalView,
    tstats: Res<crate::thermal::ThermalStats>,
    reactors: Query<(&crate::building::Footprint, &crate::thermal::ThermalState)>,
    fabs: Query<(&crate::thermal::ThermalState,), With<crate::production::Fabricator>>,
    mut texts: Query<(&mut Text, &mut TextColor), Without<Button>>,
) {
    // Summary/alert text is sim-derived; refresh on a cadence, but always
    // immediately after the overlay mode changes so [P] feels instant.
    let mode_changed = *last_mode != *overlay;
    *last_mode = *overlay;
    *ui_acc += time.delta_secs();
    if !mode_changed && *ui_acc < UI_REFRESH_SECS {
        return;
    }
    *ui_acc = 0.0;

    // ---- summary line (visible with the matching overlay mode) ----
    if let Ok((mut text, mut color)) = texts.get_mut(hud.power_line) {
        let mut summary = String::new();
        let summary_color = Color::srgb(0.62, 0.9, 0.8);
        match *overlay {
            OverlayMode::Power => {
                if power_state.networks.is_empty() {
                    summary = "POWER: no networks".to_string();
                } else {
                    let parts: Vec<String> = power_state
                        .networks
                        .iter()
                        .enumerate()
                        .map(|(i, net)| format!("NET {}: {}", i + 1, net.summary()))
                        .collect();
                    summary = format!("POWER | {}", parts.join(" | "));
                }
            }
            OverlayMode::Thermal => {
                let mut hottest = f32::NEG_INFINITY;
                for (foot, _) in reactors.iter() {
                    hottest = hottest.max(thermal.grid.max_footprint_temp(foot));
                }
                summary = format!(
                    "THERMAL | hottest core {:.0}°C | injected {:.0}H | radiated {:.0}H",
                    hottest, tstats.injected_total, tstats.radiated_total,
                );
            }
            OverlayMode::Coolant => {
                if thermal.coolant.networks.is_empty() {
                    summary = "COOLANT: no pipes laid".to_string();
                } else {
                    let parts: Vec<String> = thermal
                        .coolant
                        .networks
                        .iter()
                        .enumerate()
                        .map(|(i, n)| {
                            format!(
                                "NET {}: {} water {:.0} @ {:.0}°C flow {:.1} dump {:.0}H/s",
                                i + 1,
                                n.status_label(),
                                n.water,
                                n.avg_temp,
                                n.flow,
                                n.dump_rate
                            )
                        })
                        .collect();
                    summary = format!("COOLANT | {}", parts.join(" | "));
                }
            }
            OverlayMode::Compartments => {
                let comps = &thermal.comps;
                let air_note = if comps.air_groups as usize == comps.regions.len() {
                    String::new()
                } else {
                    format!(" | air regions {}", comps.air_groups)
                };
                summary = format!(
                    "COMPARTMENTS | {} structural | {} sealed | {} exposed | doors {} closed/{} open{air_note} | hover a room",
                    comps.regions.len(),
                    comps.sealed_count(),
                    comps.exposed_count(),
                    comps.doors_closed,
                    comps.doors_open,
                );
            }
            OverlayMode::Atmosphere => {
                let a = &thermal.atmo_summary;
                let exposed = thermal.comps.exposed_count();
                summary = format!(
                    "ATMOSPHERE | pressure {:.0}–{:.0} kPa | O2 {:.1}–{:.1} kPa | retained {:.0}% | exposed {} | active {} | hover a tile",
                    a.min_pressure,
                    a.max_pressure,
                    a.min_o2_partial,
                    a.max_o2_partial,
                    a.retained * 100.0,
                    exposed,
                    a.active_cells,
                );
            }
            OverlayMode::Off => {}
        }
        set_text_if_changed(&mut text, summary);
        if color.0 != summary_color {
            color.0 = summary_color;
        }
    }

    // ---- atmosphere alert (visible even without the overlay) ----
    // Atmosphere loss outranks thermal warnings: air going out the hull is
    // the most time-critical thing on the ship.
    let atmo_alert = thermal.atmo_summary.alert();

    // ---- thermal alert (always visible when something is wrong) ----
    let mut alert = String::new();
    let mut any_critical = false;
    if let Some(a) = atmo_alert {
        alert = a.to_string();
        any_critical = alert.starts_with("ATMOSPHERE LOSS");
    }
    for (_, state) in reactors.iter() {
        match state {
            crate::thermal::ThermalState::Critical => {
                any_critical = true;
                alert = "REACTOR THERMAL CRITICAL — emergency power only".into();
            }
            crate::thermal::ThermalState::Overheat if alert.is_empty() => {
                alert = "REACTOR OVERHEAT — derated, check cooling".into()
            }
            crate::thermal::ThermalState::Overheat | crate::thermal::ThermalState::Normal => {}
        }
    }
    for (state,) in fabs.iter() {
        if let crate::thermal::ThermalState::Critical = state {
            any_critical = true;
            if alert.is_empty() {
                alert = "FABRICATOR THERMAL CRITICAL — production stopped".into();
            }
        }
    }
    if let Ok((mut text, mut color)) = texts.get_mut(hud.alert_line) {
        set_text_if_changed(&mut text, alert);
        let want_c = if any_critical {
            Color::srgb(1.0, 0.3, 0.25)
        } else {
            Color::srgb(1.0, 0.65, 0.25)
        };
        if color.0 != want_c {
            color.0 = want_c;
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
    racks: &Query<(
        Entity,
        &TilePos,
        &StorageCell,
        Option<&Building>,
        Option<&MarkedForDeconstruct>,
    )>,
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
                            .and_then(|r| racks.get(r).ok().map(|(_, p, s, _, _)| (p, s)))
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
        temp_q: u16,
    },
    Fab {
        e: Entity,
        demo: bool,
        repeat: bool,
        ordered: bool,
    },
    Generator {
        e: Entity,
        on: bool,
        demo: bool,
        temp_q: u16,
    },
    Door {
        e: Entity,
        /// DoorMode index (0 Auto, 1 HoldOpen, 2 LockClosed).
        mode: u8,
        demo: bool,
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
    clock: Res<crate::simtime::SimClock>,
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
        &PowerStatus,
        Option<&MarkedForDeconstruct>,
    )>,
    // Fabricators also carry Building; excluding them here lets the fabs
    // branch below show the order panel instead of the generic building one.
    buildings: Query<
        (Entity, &TilePos, &Building, Option<&MarkedForDeconstruct>),
        (
            With<Building>,
            Without<StorageCell>,
            Without<crate::production::Fabricator>,
        ),
    >,
    generators: Query<(
        Entity,
        &TilePos,
        &PowerRole,
        &PowerStatus,
        Option<&MarkedForDeconstruct>,
        Option<&crate::thermal::ThermalState>,
    )>,
    power_state: Res<PowerState>,
    thermal: ThermalView,
    mut texts: Query<(&mut Text, &mut TextColor, &mut Visibility), Without<Button>>,
    mut btn_q: Query<
        (&Interaction, &mut BackgroundColor, &mut Visibility),
        (
            With<Button>,
            Without<SpeedIndex>,
            Without<ToolIndex>,
            Without<BuildCat>,
        ),
    >,
    mut last_sig: Local<SelSig>,
) {
    let now = clock.now();
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
            // The generators query matches every powered machine; only a
            // real Generator role takes the reactor branch, consumers
            // (pumps, fabricators, …) fall through to the panels below.
            let reactor = generators
                .get(e)
                .ok()
                .and_then(|(_, pos, role, _, demo, _)| {
                    if let PowerRole::Generator { on, .. } = *role {
                        Some((pos, on, demo))
                    } else {
                        None
                    }
                });
            if let Some((pos, on, demo)) = reactor {
                SelSig::Generator {
                    e,
                    on,
                    demo: demo.is_some(),
                    temp_q: thermal.grid.amb_at(*pos).max(0.0) as u16,
                }
            } else if let Ok(door) = thermal.doors.get(e) {
                SelSig::Door {
                    e,
                    mode: match door.mode {
                        crate::airtight::DoorMode::Auto => 0,
                        crate::airtight::DoorMode::HoldOpen => 1,
                        crate::airtight::DoorMode::LockClosed => 2,
                    },
                    demo: buildings
                        .get(e)
                        .ok()
                        .is_some_and(|(_, _, _, d)| d.is_some()),
                }
            } else if let Ok((_, pos, b, demo)) = buildings.get(e) {
                SelSig::Building {
                    e,
                    kind: b.kind,
                    demo: demo.is_some(),
                    temp_q: thermal.grid.amb_at(*pos).max(0.0) as u16,
                }
            } else if let Ok((_, _, cell, _, demo)) = racks_full.get(e) {
                SelSig::Rack {
                    e,
                    demo: demo.is_some(),
                    allowed: cell.allowed,
                }
            } else if let Ok((_, _, f, _, demo)) = fabs.get(e) {
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
                lines.push((task_label(task, &items, &racks_full), Color::WHITE));
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
                                    .and_then(|r| {
                                        racks_full.get(r).ok().map(|(_, p, s, _, _)| (p, s))
                                    })
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
                            format!(
                                "Unreachable (retry in {})",
                                crate::simtime::format_sim_duration(c.0 - now)
                            ),
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
            let reactor =
                generators
                    .get(e)
                    .ok()
                    .and_then(|(_, pos, role, status, demo, tstate)| {
                        if let PowerRole::Generator { output, on } = *role {
                            Some((pos, output, on, status, demo, tstate))
                        } else {
                            None
                        }
                    });
            if let Some((pos, output, on, status, demo, tstate)) = reactor {
                lines.push((
                    format!("Starter Reactor ({},{})", pos.x, pos.y),
                    Color::srgb(0.6, 1.0, 0.75),
                ));
                lines.push((
                    format!(
                        "Output: {output} PU | Status: {} | Grid: {}",
                        if on { "online" } else { "standby" },
                        status.label(),
                    ),
                    if status.ok() && on {
                        Color::srgb(0.55, 1.0, 0.65)
                    } else {
                        Color::srgb(1.0, 0.6, 0.45)
                    },
                ));
                // Thermal readout: core temperature, state, derate reason.
                let temp = thermal.grid.amb_at(*pos);
                let tstate = tstate.copied().unwrap_or_default();
                lines.push((
                    format!(
                        "Core: {:.0}°C — {} | heat follows load",
                        temp,
                        tstate.label()
                    ),
                    match tstate {
                        crate::thermal::ThermalState::Normal => Color::WHITE,
                        crate::thermal::ThermalState::Overheat => Color::srgb(1.0, 0.7, 0.25),
                        crate::thermal::ThermalState::Critical => Color::srgb(1.0, 0.4, 0.3),
                    },
                ));
                if tstate == crate::thermal::ThermalState::Critical {
                    lines.push((
                        "CRITICAL: emergency power only (pumps stay online). \
                         Restore cooling — View [P] → Coolant."
                            .to_string(),
                        Color::srgb(1.0, 0.6, 0.45),
                    ));
                }
                if let Some(net) = power_state.device_net.get(&e) {
                    if let Some(info) = power_state.networks.get(*net) {
                        lines.push((
                            format!("Network {}: {}", net + 1, info.summary()),
                            Color::WHITE,
                        ));
                        lines.push((
                            format!("Status: {}", info.status_label()),
                            if info.generation == 0 || info.demand > info.generation {
                                Color::srgb(1.0, 0.6, 0.45)
                            } else {
                                Color::srgb(0.55, 1.0, 0.65)
                            },
                        ));
                    }
                }
                if demo.is_some() {
                    lines.push((
                        "MARKED FOR DECONSTRUCTION".into(),
                        Color::srgb(1.0, 0.7, 0.25),
                    ));
                }
                lines.push((
                    "Toggle the reactor below; inspect cables with Power [P].".to_string(),
                    Color::srgb(0.6, 0.66, 0.72),
                ));
            } else if let (Ok(door), Ok((_, pos, _, demo))) =
                (thermal.doors.get(e), buildings.get(e))
            {
                lines.push((
                    format!("Door ({},{})", pos.x, pos.y),
                    Color::srgb(0.65, 0.9, 1.0),
                ));
                lines.push((
                    format!(
                        "State: {} | Mode: {}",
                        door.phase.label(),
                        door.mode.label()
                    ),
                    match door.mode {
                        crate::airtight::DoorMode::LockClosed => Color::srgb(1.0, 0.55, 0.45),
                        _ => Color::WHITE,
                    },
                ));
                lines.push((
                    format!(
                        "Passage: {} ({})",
                        door.axis.label(),
                        match door.axis {
                            crate::airtight::DoorAxis::Ns => "walls east+west",
                            crate::airtight::DoorAxis::Ew => "walls north+south",
                        }
                    ),
                    Color::srgb(0.75, 0.78, 0.82),
                ));
                // Adjacent compartments + current airtight connectivity.
                let portal = thermal
                    .comps
                    .doors
                    .iter()
                    .find(|p| p.entity == Some(e) || p.pos == *pos);
                let region_label = |id: u16| {
                    if id == crate::airtight::NO_REGION {
                        "structure".to_string()
                    } else {
                        format!("Compartment {}", id + 1)
                    }
                };
                if let Some(p) = portal {
                    let (a, b) = (p.side_a, p.side_b);
                    let joined = a != crate::airtight::NO_REGION
                        && b != crate::airtight::NO_REGION
                        && thermal.comps.air_group[a as usize]
                            == thermal.comps.air_group[b as usize];
                    lines.push((
                        format!(
                            "Sides: {} {} {}",
                            region_label(a),
                            if joined { "<-air-linked" } else { "| sealed |" },
                            region_label(b)
                        ),
                        if joined {
                            Color::srgb(0.6, 1.0, 0.7)
                        } else {
                            Color::srgb(1.0, 0.75, 0.45)
                        },
                    ));
                    lines.push((
                        format!(
                            "Airtight: {}",
                            if door.sealed() {
                                "sealed boundary"
                            } else {
                                "open — air flows"
                            }
                        ),
                        Color::WHITE,
                    ));
                }
                if demo.is_some() {
                    lines.push((
                        "MARKED FOR DECONSTRUCTION".into(),
                        Color::srgb(1.0, 0.7, 0.25),
                    ));
                }
                lines.push((
                    "Set the door mode below; View [P] → Compartments.".to_string(),
                    Color::srgb(0.6, 0.66, 0.72),
                ));
            } else if let Ok((_, pos, b, demo)) = buildings.get(e) {
                lines.push((
                    format!("{} ({},{})", b.kind.label(), pos.x, pos.y),
                    Color::srgb(0.85, 0.85, 0.9),
                ));
                let net = thermal
                    .coolant
                    .device_net
                    .get(&e)
                    .and_then(|&n| thermal.coolant.networks.get(n).copied());
                match b.kind {
                    BuildingKind::Pump => {
                        if let Some(n) = net {
                            lines.push((
                                format!(
                                    "Loop: {} | flow {:.1} ({} pump{} on)",
                                    n.status_label(),
                                    n.flow,
                                    n.powered_pumps,
                                    if n.powered_pumps == 1 { "" } else { "s" }
                                ),
                                if n.powered_pumps > 0 {
                                    Color::WHITE
                                } else {
                                    Color::srgb(1.0, 0.6, 0.45)
                                },
                            ));
                        }
                    }
                    BuildingKind::HeatExchanger => {
                        let tw = thermal.water.temp_at(*pos);
                        let w = thermal.water.amount_at(*pos);
                        if let Some(n) = net {
                            lines.push((
                                format!(
                                    "Pickup: {:.0}H/s | water {:.1} @ {:.0}°C",
                                    n.pickup_rate, w, tw
                                ),
                                Color::WHITE,
                            ));
                        }
                    }
                    BuildingKind::Radiator => {
                        let tw = thermal.water.temp_at(*pos);
                        if let Some(n) = net {
                            lines.push((
                                format!(
                                    "Dumping: {:.0}H/s | water at {:.0}°C",
                                    n.dump_rate / (n.radiators.max(1) as f32),
                                    tw
                                ),
                                Color::WHITE,
                            ));
                        }
                    }
                    BuildingKind::Reservoir => {
                        let w = thermal.water.amount_at(*pos);
                        lines.push((
                            format!(
                                "Stored: {:.0}/{:.0} water @ {:.0}°C",
                                w,
                                crate::coolant::PIPE_TILE_CAP + crate::coolant::RESERVOIR_ADD_CAP,
                                thermal.water.temp_at(*pos)
                            ),
                            Color::WHITE,
                        ));
                    }
                    _ => {}
                }
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
            } else if let Ok((_, pos, f, power, demo)) = fabs.get(e) {
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
                if !power.ok() {
                    lines.push((
                        format!("POWER: {} — machine halted", power.label()),
                        Color::srgb(1.0, 0.5, 0.4),
                    ));
                }
                if !power.ok() {
                    lines.push((
                        format!("POWER: {} — machine halted", power.label()),
                        Color::srgb(1.0, 0.5, 0.4),
                    ));
                }
                if demo.is_some() {
                    lines.push((
                        "MARKED FOR DECONSTRUCTION".into(),
                        Color::srgb(1.0, 0.7, 0.25),
                    ));
                }
                lines.push((
                    "Recipe: 2 Ore -> 1 Part (6 ship minutes)".to_string(),
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
                if text.0 != lines[i].0 {
                    text.0 = lines[i].0.clone();
                }
                if color.0 != lines[i].1 {
                    color.0 = lines[i].1;
                }
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
            SelSig::Generator { e, on, demo, .. } => {
                let mut v = vec![BtnCfg::new(
                    if *on { "Standby" } else { "Online" },
                    Action::SetGeneratorOn { gen: *e, on: !*on },
                )
                .active(*on)];
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
            SelSig::Door { e, mode, demo } => {
                let current = match mode {
                    1 => crate::airtight::DoorMode::HoldOpen,
                    2 => crate::airtight::DoorMode::LockClosed,
                    _ => crate::airtight::DoorMode::Auto,
                };
                let mut v: Vec<BtnCfg> = crate::airtight::DoorMode::ALL
                    .iter()
                    .map(|&m| {
                        BtnCfg::new(m.label(), Action::SetDoorMode { door: *e, mode: m })
                            .active(m == current)
                    })
                    .collect();
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
    // Real time: the UI refresh cadence is wall-clock (the virtual clock
    // runs at BASE_SIM_RATE × game speed and pauses with the game).
    time: Res<Time<Real>>,
    mut ui_acc: Local<f32>,
    speed: Res<GameSpeed>,
    clock: Res<crate::simtime::SimClock>,
    stats: Res<crate::stats::Stats>,
    log: Res<EventLog>,
    build_mode: Res<BuildMode>,
    mut speed_btn_q: Query<
        (&SpeedIndex, &Interaction, &mut BackgroundColor),
        (With<SpeedIndex>, Without<BuildCat>),
    >,
    mut tool_btn_q: Query<
        (&ToolIndex, &Interaction, &mut BackgroundColor),
        (With<ToolIndex>, Without<SpeedIndex>, Without<BuildCat>),
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
    racks: Query<(
        Entity,
        &TilePos,
        &StorageCell,
        Option<&Building>,
        Option<&MarkedForDeconstruct>,
    )>,
    mut texts: Query<(&mut Text, &mut TextColor, &mut Visibility), Without<Button>>,
) {
    // ---- per-frame: button highlight states (write only on change) ----
    for (idx, interaction, mut bg) in speed_btn_q.iter_mut() {
        let want = if speed.index == idx.0 {
            BUTTON_ACTIVE
        } else if *interaction == Interaction::Hovered {
            BUTTON_HOVER
        } else {
            BUTTON_BG
        };
        if bg.0 != want {
            bg.0 = want;
        }
    }

    // ---- build tool buttons + hint ----
    for (ti, interaction, mut bg) in tool_btn_q.iter_mut() {
        let active = build_mode.0 == Some(ti.0);
        let want = if active {
            BUTTON_ACTIVE
        } else if *interaction == Interaction::Hovered {
            BUTTON_HOVER
        } else {
            BUTTON_BG
        };
        if bg.0 != want {
            bg.0 = want;
        }
    }
    if let Ok((mut text, _, _)) = texts.get_mut(hud.tool_hint) {
        let want = match build_mode.0 {
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
        set_text_if_changed(&mut text, want);
    }

    // Everything below reads sim state that changes at most a few times a
    // second — refresh it on a wall-clock cadence instead of every frame.
    *ui_acc += time.delta_secs();
    if *ui_acc < UI_REFRESH_SECS {
        return;
    }
    *ui_acc = 0.0;

    // ---- sim scheduler telemetry (debug row) ----
    if let Ok((mut text, _, _)) = texts.get_mut(hud.sim_telemetry) {
        set_text_if_changed(
            &mut text,
            format!(
                "| SIM steps/frame {} peak {} backlog {:.2}s base {}/s",
                clock.steps_last_frame,
                clock.peak_steps,
                clock.backlog_secs(),
                crate::simtime::BASE_SIM_RATE,
            ),
        );
    }

    // ---- SHIP TIME ----
    if let Ok((mut text, _, _)) = texts.get_mut(hud.ship_time) {
        set_text_if_changed(
            &mut text,
            format!(
                "SHIP TIME {}",
                crate::simtime::format_sim_stamp(clock.now())
            ),
        );
    }

    // ---- stats line ----
    let marked = items.iter().filter(|(.., m, _, _, _)| m.is_some()).count();
    let stored: u32 = racks.iter().map(|(_, _, s, _, _)| s.stored()).sum();
    let cap: u32 = racks.iter().map(|(_, _, s, _, _)| s.capacity).sum();
    let idle = crews
        .iter()
        .filter(|(_, _, t, ..)| matches!(t, CrewTask::Idle(_)))
        .count();
    if let Ok((mut text, mut color, _)) = texts.get_mut(hud.stats) {
        let want = format!(
            "Marked: {marked} | Storage: {stored}/{cap} | Parts {} | Built {} | Idle {}/4",
            stats.produced, stats.built, idle,
        );
        set_text_if_changed(&mut text, want);
        let want_c = if cap == stored {
            Color::srgb(1.0, 0.45, 0.4)
        } else {
            Color::WHITE
        };
        if color.0 != want_c {
            color.0 = want_c;
        }
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
            set_text_if_changed(&mut text, line);
            if color.0 != crew.tint {
                color.0 = crew.tint;
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
                let want = format!("[{}] {}", crate::simtime::format_sim_stamp(e.time), e.text);
                set_text_if_changed(&mut text, want);
                let want_c = match e.kind {
                    LogKind::Info => Color::srgb(0.68, 0.72, 0.78),
                    LogKind::Job => Color::srgb(0.65, 0.9, 0.7),
                    LogKind::Fail => Color::srgb(1.0, 0.55, 0.45),
                };
                if color.0 != want_c {
                    color.0 = want_c;
                }
            } else {
                set_text_if_changed(&mut text, String::new());
            }
        }
    }
}
