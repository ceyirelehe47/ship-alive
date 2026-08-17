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
use crate::crew::{Crew, CrewTask, HaulPhase};
use crate::input::{BuildMode, Selected, Selection, Tool};
use crate::items::{CarriedBy, Item, ItemKind, MarkedForHaul, NoPathUntil, ReservedBy};
use crate::jobs::Action;
use crate::loc::{self, strings, Lang, Strings};
use crate::log::{EventLog, LogKind};
use crate::map::TilePos;
use crate::power::{PowerRole, PowerState, PowerStatus};
use crate::settings::StaticLabel;
use crate::storage::StorageCell;
use crate::thermal::ThermalGrid;
use crate::time_ctrl::GameSpeed;
use crate::OverlayMode;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

pub(crate) const PANEL_BG: Color = Color::srgba(0.06, 0.08, 0.11, 0.88);
pub(crate) const BUTTON_BG: Color = Color::srgba(0.22, 0.27, 0.34, 1.0);
pub(crate) const BUTTON_ACTIVE: Color = Color::srgba(0.95, 0.72, 0.20, 1.0);
pub(crate) const BUTTON_HOVER: Color = Color::srgba(0.34, 0.40, 0.48, 1.0);

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
    Atmosphere,
}

impl BuildCatKind {
    pub fn label_loc(self, l: &Strings) -> &'static str {
        cat_str(self, l)
    }

    pub const ALL: [BuildCatKind; 6] = [
        BuildCatKind::Structure,
        BuildCatKind::Storage,
        BuildCatKind::Machines,
        BuildCatKind::Power,
        BuildCatKind::Thermal,
        BuildCatKind::Atmosphere,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BuildCatKind::Structure => "Structure",
            BuildCatKind::Storage => "Storage",
            BuildCatKind::Machines => "Machines",
            BuildCatKind::Power => "Power",
            BuildCatKind::Thermal => "Thermal",
            BuildCatKind::Atmosphere => "Atmosphere",
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
            BuildCatKind::Atmosphere => &[
                BuildingKind::GasDuct,
                BuildingKind::Vent,
                BuildingKind::Blower,
                BuildingKind::GasTank,
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
const ENV_LINES: usize = 36;

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
pub(crate) fn set_text_if_changed(text: &mut Text, want: String) {
    if text.0 != want {
        text.0 = want;
    }
}

/// Static, language-bound label: spawns with the current language and is
/// rewritten by the settings module when the language switches.
pub(crate) fn klabel(
    parent: &mut ChildSpawnerCommands,
    lang: Lang,
    sel: impl Fn(&Strings) -> &'static str + Send + Sync + 'static,
    size: f32,
    color: Color,
) -> Entity {
    parent
        .spawn((
            Text::new(sel(strings(lang))),
            TextFont {
                font_size: size,
                ..default()
            },
            TextColor(color),
            StaticLabel::new(sel),
        ))
        .id()
}

/// Button whose caption is a language-bound static label.
pub(crate) fn kbutton(
    parent: &mut ChildSpawnerCommands,
    lang: Lang,
    sel: impl Fn(&Strings) -> &'static str + Send + Sync + 'static,
    action: Action,
    width: f32,
) -> Entity {
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
            b.spawn((
                Text::new(sel(strings(lang))),
                TextFont {
                    font_size: 13.0,
                    ..default()
                },
                TextColor(Color::WHITE),
                StaticLabel::new(sel),
            ));
        })
        .id()
}

pub(crate) fn label(
    parent: &mut ChildSpawnerCommands,
    text: &str,
    size: f32,
    color: Color,
) -> Entity {
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
    pub vent: Res<'w, crate::ventilation::VentSummary>,
    pub power: Res<'w, PowerState>,
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

fn cat_str(cat: BuildCatKind, l: &Strings) -> &'static str {
    match cat {
        BuildCatKind::Structure => l.cat_structure,
        BuildCatKind::Storage => l.cat_storage,
        BuildCatKind::Machines => l.cat_machines,
        BuildCatKind::Power => l.cat_power,
        BuildCatKind::Thermal => l.cat_thermal,
        BuildCatKind::Atmosphere => l.cat_atmosphere,
    }
}

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DebugBarVisible>();
        app.init_resource::<BuildMenu>();
        app.init_resource::<crate::worktab::WorkTabVisible>();
        app.add_systems(
            Startup,
            (
                build_hud,
                crate::worktab::build_work_tab,
                crate::ui_overlay::build_overlay,
            ),
        );
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
                crate::worktab::work_tab_toggle_system,
                (
                    hud_update_system,
                    selection_panel_system,
                    crate::worktab::work_tab_system,
                )
                    .chain(),
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

fn build_hud(mut commands: Commands, lang: Res<Lang>) {
    let mut speed_buttons = Vec::new();
    let mut stats = Entity::PLACEHOLDER;
    let mut ship_time_label = Entity::PLACEHOLDER;
    let mut alert_line = Entity::PLACEHOLDER;
    let mut chips = Vec::new();
    let mut sel_lines = Vec::new();
    let mut sel_btns = Vec::new();
    let mut log_lines = Vec::new();
    let mut debug_row = Entity::PLACEHOLDER;
    let debug_button_label = Entity::PLACEHOLDER;
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
                    panel
                        .spawn(Node {
                            flex_direction: FlexDirection::Row,
                            column_gap: Val::Px(6.0),
                            align_items: AlignItems::Center,
                            flex_wrap: FlexWrap::Wrap,
                            ..default()
                        })
                        .with_children(|row| {
                            label(row, "SHIP ALIVE", 15.0, Color::srgb(0.95, 0.85, 0.55));
                            stats = label(row, "", 14.0, Color::WHITE);
                            kbutton(row, *lang, |s| s.btn_haul_all, Action::MarkAll, 96.0);
                            kbutton(row, *lang, |s| s.btn_cancel_all, Action::CancelAll, 104.0);
                        });

                    // Per-network power summary line (visible with the overlay).
                    power_line = label(panel, "", 12.0, Color::srgb(0.62, 0.9, 0.8));

                    // Event feed.
                    panel
                        .spawn((
                            Interaction::default(),
                            Node {
                                padding: UiRect::all(Val::Px(4.0)),
                                flex_direction: FlexDirection::Column,
                                row_gap: Val::Px(1.0),
                                ..default()
                            },
                        ))
                        .with_children(|p| {
                            klabel(
                                p,
                                *lang,
                                |s| s.hud_event_log,
                                11.0,
                                Color::srgb(0.5, 0.55, 0.62),
                            );
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
                                Action::SpawnItem {
                                    kind: ItemKind::Crate,
                                },
                                64.0,
                            );
                            button(
                                row,
                                "+Ore",
                                Action::SpawnItem {
                                    kind: ItemKind::Ore,
                                },
                                56.0,
                            );
                            button(
                                row,
                                "+Part",
                                Action::SpawnItem {
                                    kind: ItemKind::Part,
                                },
                                60.0,
                            );
                            label(
                                row,
                                "debug tools | [X] deletes the selected item",
                                11.0,
                                Color::srgb(0.55, 0.6, 0.66),
                            );
                            sim_telemetry = label(row, "", 11.0, Color::srgb(0.6, 0.8, 0.7));
                        })
                        .id();

                    klabel(
                        panel,
                        *lang,
                        |s| s.hud_controls_hint,
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
                    panel
                        .spawn(Node {
                            flex_direction: FlexDirection::Row,
                            column_gap: Val::Px(4.0),
                            align_items: AlignItems::Center,
                            ..default()
                        })
                        .with_children(|row| {
                            ship_time_label = label(row, "", 14.0, Color::srgb(0.62, 0.9, 0.8));
                            for (i, text) in ["Pause", "1x", "2x", "4x"].iter().enumerate() {
                                if i == 0 {
                                    speed_buttons.push(kbutton(
                                        row,
                                        *lang,
                                        |s| s.btn_pause,
                                        Action::SetSpeed { index: i },
                                        52.0,
                                    ));
                                } else {
                                    speed_buttons.push(button(
                                        row,
                                        text,
                                        Action::SetSpeed { index: i },
                                        52.0,
                                    ));
                                }
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
                                OnPress(Action::ToggleWorkTab),
                                crate::worktab::WorkTabButton::Toggle,
                                Node {
                                    width: Val::Px(88.0),
                                    height: Val::Px(26.0),
                                    margin: UiRect::all(Val::Px(2.0)),
                                    align_items: AlignItems::Center,
                                    justify_content: JustifyContent::Center,
                                    ..default()
                                },
                                BackgroundColor(BUTTON_BG),
                            ))
                            .with_children(|b| {
                                b.spawn((
                                    Text::new("Work [Tab]"),
                                    TextFont {
                                        font_size: 13.0,
                                        ..default()
                                    },
                                    TextColor(Color::WHITE),
                                    crate::settings::StaticLabel::new(|s| s.btn_work),
                                ));
                            });
                            row.spawn((
                                Button,
                                Interaction::default(),
                                OnPress(Action::ToggleSettings),
                                Node {
                                    width: Val::Px(98.0),
                                    height: Val::Px(26.0),
                                    margin: UiRect::all(Val::Px(2.0)),
                                    align_items: AlignItems::Center,
                                    justify_content: JustifyContent::Center,
                                    ..default()
                                },
                                BackgroundColor(BUTTON_BG),
                            ))
                            .with_children(|b| {
                                b.spawn((
                                    Text::new("Settings [O]"),
                                    TextFont {
                                        font_size: 13.0,
                                        ..default()
                                    },
                                    TextColor(Color::WHITE),
                                    crate::settings::StaticLabel::new(|s| s.btn_settings),
                                ));
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
                                b.spawn((
                                    Text::new(strings(*lang).btn_debug),
                                    TextFont {
                                        font_size: 13.0,
                                        ..default()
                                    },
                                    TextColor(Color::WHITE),
                                    StaticLabel::new(|s| s.btn_debug),
                                ));
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
                bottom
                    .spawn((
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
                                            let c = cat;
                                            r.spawn((
                                                Text::new(cat_str(c, strings(*lang))),
                                                TextFont {
                                                    font_size: 11.0,
                                                    ..default()
                                                },
                                                TextColor(Color::srgb(0.55, 0.62, 0.7)),
                                                StaticLabel::new(move |s| cat_str(c, s)),
                                            ));
                                            for kind in cat.kinds() {
                                                let tool = Tool::Build(*kind);
                                                let e = kbutton(
                                                    r,
                                                    *lang,
                                                    move |s| loc::building_label(*kind, s),
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
                                                        BuildingKind::GasDuct => 86.0,
                                                        BuildingKind::Vent => 60.0,
                                                        BuildingKind::Blower => 72.0,
                                                        BuildingKind::GasTank => 82.0,
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
                        panel
                            .spawn((
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
                                            let c = cat;
                                            b.spawn((
                                                Text::new(cat_str(c, strings(*lang))),
                                                TextFont {
                                                    font_size: 13.0,
                                                    ..default()
                                                },
                                                TextColor(Color::WHITE),
                                                StaticLabel::new(move |s| cat_str(c, s)),
                                            ));
                                        })
                                        .id();
                                    build_cat_buttons.push((cat, e));
                                }
                                let demo_tool = Tool::Deconstruct;
                                let e = kbutton(
                                    row,
                                    *lang,
                                    |s| s.btn_deconstruct,
                                    Action::SetTool {
                                        tool: Some(demo_tool),
                                    },
                                    100.0,
                                );
                                pending_tool_btns.push((e, demo_tool));
                                tool_buttons.push(e);
                                kbutton(
                                    row,
                                    *lang,
                                    |s| s.btn_cancel_tool,
                                    Action::SetTool { tool: None },
                                    110.0,
                                );
                            });
                    });

                // Crew chips (bottom-center): basis 0 so they only ever take
                // the space the corner panels leave, wrapping as needed.
                bottom
                    .spawn((
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
                bottom
                    .spawn((
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
                        klabel(
                            sb,
                            *lang,
                            |s| s.hud_ship_status,
                            12.0,
                            Color::srgb(0.5, 0.55, 0.62),
                        );

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
    lang: Res<Lang>,
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
    // Nested tuple = one system param (the flat list would exceed 16).
    (reactors, racks, items, fabs, crews): (
        Query<(&crate::building::Footprint, &crate::thermal::ThermalState)>,
        Query<&StorageCell>,
        Query<(&Item, Option<&MarkedForHaul>), With<Item>>,
        Query<&PowerStatus, With<crate::production::Fabricator>>,
        Query<&CrewTask, With<Crew>>,
    ),
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
        let l = strings(*lang);
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
                crate::tfmt!(
                    l.fmt_env_time,
                    time = ship_time,
                    speed = loc::speed_label_loc(speed.index, l)
                ),
                Color::WHITE,
            ),
            (String::new(), Color::WHITE),
            (l.env_power.to_string(), dim),
        ];
        if power_state.networks.is_empty() {
            lines.push((l.env_no_networks.into(), dim));
        }
        for (i, net) in power_state.networks.iter().enumerate().take(3) {
            lines.push((
                crate::tfmt!(
                    l.fmt_env_net,
                    i = i + 1,
                    summary = loc::power_net_summary(net, l)
                ),
                {
                    if net.generation == 0 || net.demand > net.generation {
                        Color::srgb(1.0, 0.6, 0.45)
                    } else {
                        Color::srgb(0.7, 0.95, 0.75)
                    }
                },
            ));
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
        lines.push((l.env_thermal.to_string(), dim));
        if reactors.is_empty() {
            lines.push((l.env_no_reactor.into(), dim));
        } else {
            lines.push((
                crate::tfmt!(
                    l.fmt_env_core,
                    t = format!("{hottest_core:.0}"),
                    state = loc::thermal_state_label(worst_state, l)
                ),
                match worst_state {
                    crate::thermal::ThermalState::Normal => Color::srgb(0.7, 0.95, 0.75),
                    crate::thermal::ThermalState::Overheat => Color::srgb(1.0, 0.7, 0.25),
                    crate::thermal::ThermalState::Critical => Color::srgb(1.0, 0.4, 0.3),
                },
            ));
            lines.push((
                crate::tfmt!(l.fmt_env_hottest, t = format!("{ship_max:.0}")),
                Color::WHITE,
            ));
        }
        if thermal.coolant.networks.is_empty() {
            lines.push((l.env_coolant_none.into(), dim));
        } else {
            let dumping: f32 = thermal.coolant.networks.iter().map(|n| n.dump_rate).sum();
            lines.push((
                crate::tfmt!(
                    l.fmt_env_coolant,
                    n = thermal.coolant.networks.len(),
                    w = format!("{total_water:.0}"),
                    d = format!("{dumping:.0}")
                ),
                Color::WHITE,
            ));
        }
        // Airtight compartments block (Slice 4).
        lines.push((String::new(), Color::WHITE));
        lines.push((l.env_compartments.to_string(), dim));
        let comps = &thermal.comps;
        let exposed = comps.exposed_count();
        lines.push((
            crate::tfmt!(
                l.fmt_env_comps,
                n = comps.regions.len(),
                s = comps.sealed_count(),
                e = exposed
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
            crate::tfmt!(l.fmt_env_air_note, n = comps.air_groups)
        };
        lines.push((
            crate::tfmt!(
                l.fmt_env_doors,
                c = comps.doors_closed,
                o = comps.doors_open,
                note = air_note
            ),
            Color::WHITE,
        ));
        // Atmosphere block (Slice 5) — reads the cached summary only, never
        // a per-frame grid scan.
        lines.push((String::new(), Color::WHITE));
        lines.push((l.env_atmosphere.to_string(), dim));
        let a = &thermal.atmo_summary;
        let exposed = comps.exposed_count();
        lines.push((
            crate::tfmt!(
                l.fmt_env_pressure,
                min = format!("{:.0}", a.min_pressure),
                max = format!("{:.0}", a.max_pressure)
            ),
            if a.low_cells > 0 || a.vacuum_cells > 0 {
                Color::srgb(1.0, 0.45, 0.35)
            } else {
                Color::WHITE
            },
        ));
        lines.push((
            crate::tfmt!(
                l.fmt_env_o2,
                min = format!("{:.1}", a.min_o2_partial),
                max = format!("{:.1}", a.max_o2_partial)
            ),
            if a.low_o2_cells > 0 {
                Color::srgb(1.0, 0.6, 0.4)
            } else {
                Color::WHITE
            },
        ));
        lines.push((
            crate::tfmt!(
                l.fmt_env_retained,
                p = format!("{:.0}", a.retained * 100.0),
                note = if a.max_co2_partial > crate::atmosphere::CO2_HIGH_KPA {
                    crate::tfmt!(l.fmt_env_co2_note, v = format!("{:.1}", a.max_co2_partial))
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
            crate::tfmt!(
                l.fmt_env_exposed,
                n = exposed,
                note = if a.active_cells > 0 {
                    crate::tfmt!(l.fmt_env_venting_note, n = a.active_cells)
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
        // Ventilation block (Slice 6) — cached summary only.
        let v = &thermal.vent;
        lines.push((String::new(), Color::WHITE));
        lines.push((l.env_ventilation.to_string(), dim));
        lines.push((
            crate::tfmt!(
                l.fmt_env_vent_nets,
                n = v.networks,
                a = v.active_cells,
                g = format!("{:.0}", v.stored_mol)
            ),
            Color::WHITE,
        ));
        lines.push((
            crate::tfmt!(
                l.fmt_env_blowers,
                on = v.blowers_on,
                total = v.blowers_total,
                note = if v.max_tank_p > 0.0 {
                    crate::tfmt!(l.fmt_env_tanks_note, p = format!("{:.0}", v.max_tank_p))
                } else {
                    String::new()
                }
            ),
            match loc::vent_alert_label(v, l) {
                Some(_) => Color::srgb(1.0, 0.55, 0.45),
                None => Color::WHITE,
            },
        ));
        if let Some(a) = loc::vent_alert_label(v, l) {
            lines.push((a.to_string(), Color::srgb(1.0, 0.55, 0.45)));
        }
        lines.push((String::new(), Color::WHITE));
        lines.push((l.env_storage.to_string(), dim));
        lines.push((
            crate::tfmt!(
                l.fmt_env_stored,
                stored = stored,
                cap = cap,
                full = if cap == stored {
                    l.storage_full_suffix
                } else {
                    ""
                }
            ),
            if cap == stored {
                Color::srgb(1.0, 0.45, 0.4)
            } else {
                Color::WHITE
            },
        ));
        lines.push((
            crate::tfmt!(
                l.fmt_env_racks,
                o = rack_counts[ItemKind::Ore.index()],
                p = rack_counts[ItemKind::Part.index()],
                c = rack_counts[ItemKind::Crate.index()]
            ),
            Color::WHITE,
        ));
        lines.push((
            crate::tfmt!(
                l.fmt_env_ground,
                o = ground[ItemKind::Ore.index()],
                p = ground[ItemKind::Part.index()],
                c = ground[ItemKind::Crate.index()]
            ),
            Color::WHITE,
        ));
        lines.push((crate::tfmt!(l.fmt_env_marked, n = marked), Color::WHITE));
        lines.push((String::new(), Color::WHITE));
        lines.push((l.env_production.to_string(), dim));
        lines.push((
            crate::tfmt!(l.fmt_env_parts, p = stats.produced, b = stats.built),
            Color::WHITE,
        ));
        lines.push((
            crate::tfmt!(l.fmt_env_fabs, on = fabs_online, total = fabs_total),
            if fabs_online < fabs_total {
                Color::srgb(1.0, 0.6, 0.45)
            } else {
                Color::WHITE
            },
        ));
        lines.push((
            crate::tfmt!(
                l.fmt_env_idle,
                idle = idle,
                total = crews.iter().count(),
                h = stats.hauls_done
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
#[allow(clippy::too_many_arguments)]
fn overlay_cycle_system(
    mut events: EventReader<Action>,
    mut overlay: ResMut<OverlayMode>,
    hud: Res<Hud>,
    lang: Res<Lang>,
    mut last_lang: Local<Lang>,
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
    let lang_changed = *last_lang != *lang;
    if !cycled && !lang_changed {
        return;
    }
    *last_lang = *lang;
    let l = strings(*lang);
    if cycled {
        log.push(
            clock.now(),
            LogKind::Info,
            crate::tfmt!(l.fmt_log_overlay, mode = loc::overlay_label(*overlay, l)),
        );
    }
    if let Ok(mut text) = text_q.get_mut(hud.power_button_label) {
        text.0 = crate::tfmt!(l.btn_view, mode = loc::overlay_label(*overlay, l));
    }
}

/// Overlay summary line under the bar + the always-on thermal alert.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn overlay_summary_system(
    hud: Res<Hud>,
    lang: Res<Lang>,
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
    let l = strings(*lang);

    // ---- summary line (visible with the matching overlay mode) ----
    if let Ok((mut text, mut color)) = texts.get_mut(hud.power_line) {
        let mut summary = String::new();
        let summary_color = Color::srgb(0.62, 0.9, 0.8);
        match *overlay {
            OverlayMode::Power => {
                if power_state.networks.is_empty() {
                    summary = l.ov_power_none.to_string();
                } else {
                    let parts: Vec<String> = power_state
                        .networks
                        .iter()
                        .enumerate()
                        .map(|(i, net)| {
                            crate::tfmt!(
                                l.fmt_env_net,
                                i = i + 1,
                                summary = loc::power_net_summary(net, l)
                            )
                        })
                        .collect();
                    summary = crate::tfmt!(l.ov_power, nets = parts.join(" | "));
                }
            }
            OverlayMode::Thermal => {
                let mut hottest = f32::NEG_INFINITY;
                for (foot, _) in reactors.iter() {
                    hottest = hottest.max(thermal.grid.max_footprint_temp(foot));
                }
                summary = crate::tfmt!(
                    l.fmt_ov_thermal,
                    t = format!("{hottest:.0}"),
                    i = format!("{:.0}", tstats.injected_total),
                    r = format!("{:.0}", tstats.radiated_total)
                );
            }
            OverlayMode::Coolant => {
                if thermal.coolant.networks.is_empty() {
                    summary = l.ov_coolant_none.to_string();
                } else {
                    let parts: Vec<String> = thermal
                        .coolant
                        .networks
                        .iter()
                        .enumerate()
                        .map(|(i, n)| {
                            crate::tfmt!(
                                l.fmt_ov_coolant_net,
                                i = i + 1,
                                status = loc::coolant_status_label(n, l),
                                w = format!("{:.0}", n.water),
                                t = format!("{:.0}", n.avg_temp),
                                f = format!("{:.1}", n.flow),
                                d = format!("{:.0}", n.dump_rate)
                            )
                        })
                        .collect();
                    summary = crate::tfmt!(l.ov_coolant, nets = parts.join(" | "));
                }
            }
            OverlayMode::Compartments => {
                let comps = &thermal.comps;
                let air_note = if comps.air_groups as usize == comps.regions.len() {
                    String::new()
                } else {
                    crate::tfmt!(l.fmt_env_air_note, n = comps.air_groups)
                };
                summary = crate::tfmt!(
                    l.fmt_ov_comps,
                    s = comps.regions.len(),
                    se = comps.sealed_count(),
                    e = comps.exposed_count(),
                    dc = comps.doors_closed,
                    doors_open = comps.doors_open,
                    note = air_note
                );
            }
            OverlayMode::Ventilation => {
                let v = &thermal.vent;
                summary = crate::tfmt!(
                    l.fmt_ov_vent,
                    n = v.networks,
                    plural = if v.networks == 1 {
                        ""
                    } else {
                        l.ov_vent_plural
                    },
                    g = format!("{:.0}", v.stored_mol),
                    a = v.active_cells,
                    on = v.blowers_on,
                    total = v.blowers_total,
                    alert = if loc::vent_alert_label(v, l).is_some() {
                        crate::tfmt!(l.fmt_env_tanks_note, p = format!("{:.0}", v.max_tank_p))
                    } else {
                        String::new()
                    },
                    p = format!("{:.0}", v.max_tank_p)
                );
            }
            OverlayMode::Atmosphere => {
                let a = &thermal.atmo_summary;
                let exposed = thermal.comps.exposed_count();
                summary = crate::tfmt!(
                    l.fmt_ov_atmo,
                    pmin = format!("{:.0}", a.min_pressure),
                    pmax = format!("{:.0}", a.max_pressure),
                    omin = format!("{:.1}", a.min_o2_partial),
                    omax = format!("{:.1}", a.max_o2_partial),
                    r = format!("{:.0}", a.retained * 100.0),
                    e = exposed,
                    a = a.active_cells
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
    let atmo_alert = loc::atmo_alert_label(&thermal.atmo_summary, l);
    let vent_alert = loc::vent_alert_label(&thermal.vent, l);

    // ---- thermal alert (always visible when something is wrong) ----
    let mut alert = String::new();
    let mut any_critical = false;
    if let Some(a) = atmo_alert {
        alert = a.to_string();
        any_critical = thermal.atmo_summary.vacuum_cells > 0 || thermal.atmo_summary.low_cells > 0;
    } else if let Some(v) = vent_alert {
        alert = v.to_string();
    }
    for (_, state) in reactors.iter() {
        match state {
            crate::thermal::ThermalState::Critical => {
                any_critical = true;
                alert = l.alert_reactor_crit.into();
            }
            crate::thermal::ThermalState::Overheat if alert.is_empty() => {
                alert = l.alert_reactor_over.into()
            }
            crate::thermal::ThermalState::Overheat | crate::thermal::ThermalState::Normal => {}
        }
    }
    for (state,) in fabs.iter() {
        if let crate::thermal::ThermalState::Critical = state {
            any_critical = true;
            if alert.is_empty() {
                alert = l.alert_fab_crit.into();
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
    l: &Strings,
) -> String {
    match task {
        CrewTask::Idle(cause) => loc::idle_cause_label(cause, l),
        CrewTask::Haul(job) => {
            let kind = items
                .get(job.item)
                .map(|(_, _, i, ..)| loc::item_label(i.kind, l))
                .unwrap_or(l.generic_item)
                .to_string();
            match job.phase {
                HaulPhase::ToItem | HaulPhase::PickingUp => {
                    crate::tfmt!(
                        l.fmt_task_haul_going,
                        kind = kind,
                        dest = loc::haul_dest_label(job.dest, l)
                    )
                }
                HaulPhase::ToDest | HaulPhase::Delivering => {
                    let dest = match job.dest {
                        crate::crew::HaulDest::Storage => job
                            .target_rack
                            .and_then(|r| racks.get(r).ok().map(|(_, p, _, _, _)| *p))
                            .map(|p| crate::tfmt!(l.fmt_rack_at, x = p.x, y = p.y))
                            .unwrap_or_else(|| l.dest_storage.to_string()),
                        crate::crew::HaulDest::Blueprint(_) => l.dest_blueprint.to_string(),
                        crate::crew::HaulDest::Machine(_) => l.dest_machine.to_string(),
                    };
                    crate::tfmt!(l.fmt_task_haul_carrying, kind = kind, dest = dest)
                }
            }
        }
        CrewTask::Build(job) => {
            if job.phase == crate::crew::WorkPhase::Working {
                crate::tfmt!(
                    l.fmt_task_building,
                    t = format!("{:.0}", job.timer.max(0.0))
                )
            } else {
                l.task_going_build.to_string()
            }
        }
        CrewTask::Deconstruct(job) => {
            if job.phase == crate::crew::WorkPhase::Working {
                crate::tfmt!(l.fmt_task_demoing, t = format!("{:.0}", job.timer.max(0.0)))
            } else {
                l.task_going_demo.to_string()
            }
        }
        CrewTask::Operate(job) => {
            if job.phase == crate::crew::WorkPhase::Working {
                crate::tfmt!(
                    l.fmt_task_operating,
                    t = format!("{:.0}", job.timer.max(0.0))
                )
            } else {
                l.task_going_operate.to_string()
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
    l: &Strings,
) -> String {
    if carried.is_some() {
        let carrier = carried
            .and_then(|c| crews.get(c.0).ok())
            .map(|(_, c, ..)| c.name.clone())
            .unwrap_or_else(|| l.generic_someone.into());
        crate::tfmt!(l.fmt_item_carried, name = carrier)
    } else if let Some(r) = reserved {
        let claimer = crews
            .get(r.0)
            .map(|(_, c, ..)| c.name.clone())
            .unwrap_or_else(|_| l.generic_crew_member.into());
        let dest = crews
            .get(r.0)
            .ok()
            .and_then(|(_, _, t, ..)| match t {
                CrewTask::Haul(j) => Some(loc::haul_dest_label(j.dest, l)),
                _ => None,
            })
            .unwrap_or_else(|| l.dest_storage.into());
        crate::tfmt!(l.fmt_item_claimed, name = claimer, dest = dest)
    } else if marked.is_some() {
        l.item_marked.to_string()
    } else if cooled.is_some_and(|c| c.0 > now) {
        l.item_unreachable.to_string()
    } else {
        l.item_ground.to_string()
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
    Vent {
        e: Entity,
        /// VentMode index (0 Supply, 1 Exhaust, 2 Balanced).
        mode: u8,
        open: bool,
        demo: bool,
    },
    Blower {
        e: Entity,
        /// Dir4 index (0 E, 1 W, 2 S, 3 N).
        dir: u8,
        on: bool,
        demo: bool,
    },
    Tank {
        e: Entity,
        valve: bool,
        demo: bool,
    },
}

#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn selection_panel_system(
    hud: Res<Hud>,
    selection: Res<Selection>,
    clock: Res<crate::simtime::SimClock>,
    lang: Res<Lang>,
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
    (racks_full, blueprints, fabs): (
        Query<(
            Entity,
            &TilePos,
            &StorageCell,
            Option<&Building>,
            Option<&MarkedForDeconstruct>,
        )>,
        Query<(Entity, &TilePos, &crate::building::Blueprint)>,
        Query<(
            Entity,
            &TilePos,
            &crate::production::Fabricator,
            &PowerStatus,
            Option<&MarkedForDeconstruct>,
        )>,
    ),
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
    (vents_q, blowers_q, tanks_q, ducts): (
        Query<(Entity, &TilePos, &crate::ventilation::Vent)>,
        Query<(Entity, &TilePos, &crate::ventilation::Blower)>,
        Query<(Entity, &TilePos, &crate::ventilation::GasTank)>,
        Res<crate::ventilation::DuctGrid>,
    ),
    mut last_sig: Local<SelSig>,
    mut last_lang: Local<Lang>,
) {
    let now = clock.now();
    let l = strings(*lang);
    // ---- selection panel: text lines ----
    let mut lines: Vec<(String, Color)> = Vec::new();
    let sig = match selection.0 {
        None => SelSig::None,
        Some(Selected::Crew(e)) => match crews.get(e) {
            Ok(_) => SelSig::Crew { e },
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
            } else if let Ok((_, _, vent)) = vents_q.get(e) {
                SelSig::Vent {
                    e,
                    mode: match vent.mode {
                        crate::ventilation::VentMode::Supply => 0,
                        crate::ventilation::VentMode::Exhaust => 1,
                        crate::ventilation::VentMode::Balanced => 2,
                    },
                    open: vent.open,
                    demo: buildings
                        .get(e)
                        .ok()
                        .is_some_and(|(_, _, _, d)| d.is_some()),
                }
            } else if let Ok((_, _, blower)) = blowers_q.get(e) {
                SelSig::Blower {
                    e,
                    dir: match blower.dir {
                        crate::ventilation::Dir4::East => 0,
                        crate::ventilation::Dir4::West => 1,
                        crate::ventilation::Dir4::South => 2,
                        crate::ventilation::Dir4::North => 3,
                    },
                    on: blower.enabled,
                    demo: buildings
                        .get(e)
                        .ok()
                        .is_some_and(|(_, _, _, d)| d.is_some()),
                }
            } else if let Ok((_, _, tank)) = tanks_q.get(e) {
                SelSig::Tank {
                    e,
                    valve: tank.valve_open,
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
                    crate::tfmt!(l.fmt_sel_crew, name = crew.name, x = pos.x, y = pos.y),
                    crew.tint,
                ));
                lines.push((task_label(task, &items, &racks_full, l), Color::WHITE));
                match task {
                    CrewTask::Haul(job) => {
                        let detail = match job.phase {
                            HaulPhase::ToItem | HaulPhase::PickingUp => items
                                .get(job.item)
                                .ok()
                                .map(|(_, p, ..)| {
                                    crate::tfmt!(l.fmt_sel_target_item, x = p.x, y = p.y)
                                })
                                .unwrap_or_else(|| l.sel_target_gone.into()),
                            _ => match job.dest {
                                crate::crew::HaulDest::Storage => job
                                    .target_rack
                                    .and_then(|r| {
                                        racks_full.get(r).ok().map(|(_, p, s, _, _)| (p, s))
                                    })
                                    .map(|(p, s)| {
                                        crate::tfmt!(
                                            l.fmt_sel_deliver_rack,
                                            x = p.x,
                                            y = p.y,
                                            label = s.label()
                                        )
                                    })
                                    .unwrap_or_else(|| l.sel_no_rack.into()),
                                crate::crew::HaulDest::Blueprint(_) => l.sel_deliver_bp.into(),
                                crate::crew::HaulDest::Machine(_) => l.sel_load_fab.into(),
                            },
                        };
                        lines.push((detail, Color::srgb(0.75, 0.78, 0.82)));
                    }
                    CrewTask::Build(_) => {
                        lines.push((l.sel_target_bp.into(), Color::srgb(0.75, 0.78, 0.82)))
                    }
                    CrewTask::Deconstruct(_) => {
                        lines.push((l.sel_target_demo.into(), Color::srgb(0.75, 0.78, 0.82)))
                    }
                    CrewTask::Operate(_) => {
                        lines.push((l.sel_target_fab.into(), Color::srgb(0.75, 0.78, 0.82)))
                    }
                    CrewTask::Idle(cause) => {
                        lines.push((loc::idle_cause_label(cause, l), Color::srgb(0.7, 0.74, 0.8)));
                    }
                }
                lines.push((
                    crate::tfmt!(
                        l.fmt_sel_counts,
                        h = crew.delivered,
                        b = crew.built,
                        o = crew.operated,
                        p = mov.path.len()
                    ),
                    Color::srgb(0.6, 0.66, 0.72),
                ));
                lines.push((l.sel_work_hint.to_string(), Color::srgb(0.6, 0.66, 0.72)));
            }
        }
        Some(Selected::Item(e)) => {
            if let Ok((_, pos, item, marked, reserved, carried, cooled)) = items.get(e) {
                lines.push((
                    crate::tfmt!(
                        l.fmt_sel_item,
                        kind = loc::item_label(item.kind, l),
                        x = pos.x,
                        y = pos.y
                    ),
                    Color::srgb(0.95, 0.85, 0.55),
                ));
                lines.push((
                    item_status(reserved, carried, marked, cooled, &crews, now, l),
                    Color::WHITE,
                ));
                if let Some(c) = cooled {
                    if c.0 > now {
                        lines.push((
                            crate::tfmt!(
                                l.fmt_sel_unreachable,
                                t = crate::simtime::format_sim_duration(c.0 - now)
                            ),
                            Color::srgb(1.0, 0.45, 0.4),
                        ));
                    }
                }
                lines.push((l.sel_toggle_mark.to_string(), Color::srgb(0.6, 0.66, 0.72)));
            }
        }
        Some(Selected::Rack(e)) => {
            if let Ok((_, pos, cell, _, demo)) = racks_full.get(e) {
                lines.push((
                    crate::tfmt!(l.fmt_sel_rack, x = pos.x, y = pos.y),
                    Color::srgb(0.6, 0.9, 0.8),
                ));
                let counts = ItemKind::ALL
                    .iter()
                    .map(|k| {
                        crate::tfmt!(
                            l.fmt_sel_rack_counts,
                            kind = loc::item_label(*k, l),
                            n = cell.counts[k.index()]
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" | ");
                lines.push((counts, Color::WHITE));
                let accepts = if cell.allowed.iter().all(|&a| a) {
                    l.filter_any.to_string()
                } else {
                    ItemKind::ALL
                        .iter()
                        .filter(|k| cell.allowed[k.index()])
                        .map(|k| loc::item_short(*k, l))
                        .collect::<Vec<_>>()
                        .join("+")
                };
                lines.push((
                    crate::tfmt!(l.fmt_sel_rack_free, free = cell.free(), accepts = accepts),
                    if cell.free() == 0 {
                        Color::srgb(1.0, 0.45, 0.4)
                    } else {
                        Color::WHITE
                    },
                ));
                if demo.is_some() {
                    lines.push((l.sel_marked_demo.into(), Color::srgb(1.0, 0.7, 0.25)));
                }
                lines.push((
                    l.sel_rack_filter_hint.to_string(),
                    Color::srgb(0.6, 0.66, 0.72),
                ));
            }
        }
        Some(Selected::Blueprint(e)) => {
            if let Ok((_, pos, bp)) = blueprints.get(e) {
                lines.push((
                    crate::tfmt!(
                        l.fmt_sel_blueprint,
                        kind = loc::building_label(bp.kind, l),
                        x = pos.x,
                        y = pos.y
                    ),
                    Color::srgb(0.55, 0.85, 1.0),
                ));
                lines.push((
                    crate::tfmt!(l.fmt_sel_materials, m = bp.materials_label_loc(l)),
                    Color::WHITE,
                ));
                for (kind, miss) in bp.missing_list() {
                    lines.push((
                        crate::tfmt!(
                            l.fmt_sel_waiting_for,
                            n = miss,
                            kind = loc::item_label(kind, l)
                        ),
                        Color::srgb(1.0, 0.75, 0.4),
                    ));
                }
                if bp.fully_supplied() {
                    lines.push((
                        if bp.progress > 0.0 {
                            crate::tfmt!(l.fmt_sel_constructing, p = (bp.progress * 100.0) as u32)
                        } else {
                            l.sel_bp_ready.to_string()
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
                    crate::tfmt!(l.fmt_sel_reactor, x = pos.x, y = pos.y),
                    Color::srgb(0.6, 1.0, 0.75),
                ));
                lines.push((
                    crate::tfmt!(
                        l.fmt_sel_reactor_line,
                        out = output,
                        status = if on {
                            l.reactor_online
                        } else {
                            l.reactor_standby
                        },
                        grid = loc::power_status_label(*status, l)
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
                    crate::tfmt!(
                        l.fmt_sel_core,
                        t = format!("{temp:.0}"),
                        state = loc::thermal_state_label(tstate, l)
                    ),
                    match tstate {
                        crate::thermal::ThermalState::Normal => Color::WHITE,
                        crate::thermal::ThermalState::Overheat => Color::srgb(1.0, 0.7, 0.25),
                        crate::thermal::ThermalState::Critical => Color::srgb(1.0, 0.4, 0.3),
                    },
                ));
                if tstate == crate::thermal::ThermalState::Critical {
                    lines.push((l.sel_reactor_crit.to_string(), Color::srgb(1.0, 0.6, 0.45)));
                }
                if let Some(net) = thermal.power.device_net.get(&e) {
                    if let Some(info) = thermal.power.networks.get(*net) {
                        lines.push((
                            crate::tfmt!(
                                l.fmt_sel_net,
                                i = net + 1,
                                summary = loc::power_net_summary(info, l)
                            ),
                            Color::WHITE,
                        ));
                        lines.push((
                            crate::tfmt!(l.fmt_sel_net_status, s = info.status_label()),
                            if info.generation == 0 || info.demand > info.generation {
                                Color::srgb(1.0, 0.6, 0.45)
                            } else {
                                Color::srgb(0.55, 1.0, 0.65)
                            },
                        ));
                    }
                }
                if demo.is_some() {
                    lines.push((l.sel_marked_demo.into(), Color::srgb(1.0, 0.7, 0.25)));
                }
                lines.push((l.sel_reactor_hint.to_string(), Color::srgb(0.6, 0.66, 0.72)));
            } else if let (Ok(door), Ok((_, pos, _, demo))) =
                (thermal.doors.get(e), buildings.get(e))
            {
                lines.push((
                    crate::tfmt!(l.fmt_sel_door, x = pos.x, y = pos.y),
                    Color::srgb(0.65, 0.9, 1.0),
                ));
                lines.push((
                    crate::tfmt!(
                        l.fmt_sel_door_state,
                        state = loc::door_phase_label(door.phase, l),
                        mode = loc::door_mode_label(door.mode, l)
                    ),
                    match door.mode {
                        crate::airtight::DoorMode::LockClosed => Color::srgb(1.0, 0.55, 0.45),
                        _ => Color::WHITE,
                    },
                ));
                lines.push((
                    crate::tfmt!(
                        l.fmt_sel_door_passage,
                        axis = door.axis.label(),
                        walls = match door.axis {
                            crate::airtight::DoorAxis::Ns => l.door_walls_ew,
                            crate::airtight::DoorAxis::Ew => l.door_walls_ns,
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
                        l.sel_door_structure.to_string()
                    } else {
                        crate::tfmt!(l.fmt_sel_compartment, n = id + 1)
                    }
                };
                if let Some(p) = portal {
                    let (a, b) = (p.side_a, p.side_b);
                    let joined = a != crate::airtight::NO_REGION
                        && b != crate::airtight::NO_REGION
                        && thermal.comps.air_group[a as usize]
                            == thermal.comps.air_group[b as usize];
                    lines.push((
                        crate::tfmt!(
                            l.fmt_sel_sides,
                            a = region_label(a),
                            link = if joined {
                                l.door_air_linked
                            } else {
                                l.door_sealed_sep
                            },
                            b = region_label(b)
                        ),
                        if joined {
                            Color::srgb(0.6, 1.0, 0.7)
                        } else {
                            Color::srgb(1.0, 0.75, 0.45)
                        },
                    ));
                    lines.push((
                        crate::tfmt!(
                            l.fmt_sel_airtight,
                            s = if door.sealed() {
                                l.sel_boundary_sealed
                            } else {
                                l.sel_boundary_open
                            }
                        ),
                        Color::WHITE,
                    ));
                }
                if demo.is_some() {
                    lines.push((l.sel_marked_demo.into(), Color::srgb(1.0, 0.7, 0.25)));
                }
                lines.push((l.sel_door_hint.to_string(), Color::srgb(0.6, 0.66, 0.72)));
            } else if let (Ok((_, _, vent)), Ok((_, pos, _, _))) =
                (vents_q.get(e), buildings.get(e))
            {
                lines.push((
                    crate::tfmt!(l.fmt_sel_vent, x = pos.x, y = pos.y),
                    Color::srgb(0.65, 0.9, 1.0),
                ));
                let p_room = thermal.atmo.pressure_at(*pos, &thermal.grid);
                let p_duct = ducts.pressure_at(*pos);
                lines.push((
                    crate::tfmt!(
                        l.fmt_sel_vent_mode,
                        mode = loc::vent_mode_label(vent.mode, l),
                        open = if vent.open { l.val_open } else { l.val_closed }
                    ),
                    if vent.open {
                        Color::WHITE
                    } else {
                        Color::srgb(1.0, 0.55, 0.45)
                    },
                ));
                lines.push((
                    crate::tfmt!(
                        l.fmt_sel_vent_pressures,
                        room = format!("{p_room:.0}"),
                        duct = format!("{p_duct:.0}"),
                        rate = format!("{:.1}", vent.last_rate)
                    ),
                    Color::WHITE,
                ));
                if !ducts.has(*pos) {
                    lines.push((l.sel_no_duct_vent.into(), Color::srgb(1.0, 0.55, 0.45)));
                }
                lines.push((l.sel_vent_hint.to_string(), Color::srgb(0.6, 0.66, 0.72)));
            } else if let (Ok((_, _, blower)), Ok((_, pos, _, _))) =
                (blowers_q.get(e), buildings.get(e))
            {
                lines.push((
                    crate::tfmt!(l.fmt_sel_blower, x = pos.x, y = pos.y),
                    Color::srgb(0.65, 0.9, 1.0),
                ));
                let dd = blower.dir.delta();
                let out_p = TilePos::new(pos.x + dd.x, pos.y + dd.y);
                lines.push((
                    crate::tfmt!(
                        l.fmt_sel_blower_push,
                        dir = loc::dir4_label(blower.dir, l),
                        on = if blower.enabled { l.val_on } else { l.val_off },
                        flow = format!("{:.1}", blower.last_flow)
                    ),
                    if blower.enabled {
                        Color::WHITE
                    } else {
                        Color::srgb(1.0, 0.55, 0.45)
                    },
                ));
                lines.push((
                    crate::tfmt!(
                        l.fmt_sel_blower_p,
                        inlet = format!("{:.0}", ducts.pressure_at(*pos)),
                        outlet = format!("{:.0}", ducts.pressure_at(out_p))
                    ),
                    Color::WHITE,
                ));
                if !ducts.has(*pos) {
                    lines.push((l.sel_no_duct_blower.into(), Color::srgb(1.0, 0.55, 0.45)));
                }
            } else if let (Ok((_, _, tank)), Ok((_, pos, _, _))) =
                (tanks_q.get(e), buildings.get(e))
            {
                lines.push((
                    crate::tfmt!(l.fmt_sel_tank, x = pos.x, y = pos.y),
                    Color::srgb(0.65, 0.9, 1.0),
                ));
                let p = tank.pressure();
                lines.push((
                    crate::tfmt!(
                        l.fmt_sel_tank_valve,
                        valve = if tank.valve_open {
                            l.val_open
                        } else {
                            l.val_closed
                        },
                        p = format!("{p:.0}"),
                        units = format!("{:.0}", tank.total()),
                        cap = crate::ventilation::TANK_MOL as u32
                    ),
                    if p > crate::ventilation::TANK_HIGH_KPA {
                        Color::srgb(1.0, 0.55, 0.45)
                    } else if tank.valve_open {
                        Color::WHITE
                    } else {
                        Color::srgb(1.0, 0.7, 0.4)
                    },
                ));
                let m = &tank.mix;
                let t = m.total();
                lines.push((
                    crate::tfmt!(
                        l.fmt_sel_tank_mix,
                        o2 = format!("{:.0}", if t > 0.0 { m.mol[0] / t * 100.0 } else { 0.0 }),
                        inert = format!("{:.0}", if t > 0.0 { m.mol[1] / t * 100.0 } else { 0.0 }),
                        co2 = format!("{:.1}", if t > 0.0 { m.mol[2] / t * 100.0 } else { 0.0 }),
                        pol = format!("{:.1}", if t > 0.0 { m.mol[3] / t * 100.0 } else { 0.0 }),
                        t = format!("{:.0}", tank.temp)
                    ),
                    Color::WHITE,
                ));
                if !ducts.has(*pos) {
                    lines.push((l.sel_no_duct_tank.into(), Color::srgb(1.0, 0.55, 0.45)));
                }
            } else if let Ok((_, pos, b, demo)) = buildings.get(e) {
                lines.push((
                    crate::tfmt!(
                        l.fmt_sel_building,
                        kind = loc::building_label(b.kind, l),
                        x = pos.x,
                        y = pos.y
                    ),
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
                                crate::tfmt!(
                                    l.fmt_sel_loop,
                                    status = loc::coolant_status_label(&n, l),
                                    flow = format!("{:.1}", n.flow),
                                    pumps = n.powered_pumps,
                                    plural = if n.powered_pumps == 1 { "" } else { "s" }
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
                                crate::tfmt!(
                                    l.fmt_sel_pickup,
                                    rate = format!("{:.0}", n.pickup_rate),
                                    w = format!("{w:.1}"),
                                    t = format!("{tw:.0}")
                                ),
                                Color::WHITE,
                            ));
                        }
                    }
                    BuildingKind::Radiator => {
                        let tw = thermal.water.temp_at(*pos);
                        if let Some(n) = net {
                            lines.push((
                                crate::tfmt!(
                                    l.fmt_sel_dumping,
                                    rate =
                                        format!("{:.0}", n.dump_rate / (n.radiators.max(1) as f32)),
                                    t = format!("{tw:.0}")
                                ),
                                Color::WHITE,
                            ));
                        }
                    }
                    BuildingKind::Reservoir => {
                        let w = thermal.water.amount_at(*pos);
                        lines.push((
                            crate::tfmt!(
                                l.fmt_sel_reservoir,
                                w = format!("{w:.0}"),
                                cap = format!(
                                    "{:.0}",
                                    crate::coolant::PIPE_TILE_CAP
                                        + crate::coolant::RESERVOIR_ADD_CAP
                                ),
                                t = format!("{:.0}", thermal.water.temp_at(*pos))
                            ),
                            Color::WHITE,
                        ));
                    }
                    _ => {}
                }
                if demo.is_some() {
                    lines.push((
                        if b.demo_progress > 0.0 {
                            crate::tfmt!(l.fmt_sel_demoing, p = (b.demo_progress * 100.0) as u32)
                        } else {
                            l.sel_demo_waiting.to_string()
                        },
                        Color::srgb(1.0, 0.7, 0.25),
                    ));
                } else {
                    lines.push((l.sel_demo_hint.to_string(), Color::srgb(0.6, 0.66, 0.72)));
                }
            } else if let Ok((_, pos, f, power, demo)) = fabs.get(e) {
                let state = f.state();
                lines.push((
                    crate::tfmt!(l.fmt_sel_fab, x = pos.x, y = pos.y),
                    Color::srgb(0.75, 0.8, 1.0),
                ));
                lines.push((
                    crate::tfmt!(
                        l.fmt_sel_fab_state,
                        state = loc::machine_state_label(state, l),
                        ore = f.input[ItemKind::Ore.index()],
                        parts = f.output[ItemKind::Part.index()]
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
                    Some(o) if o.repeat => l.sel_order_repeat.to_string(),
                    Some(o) => crate::tfmt!(l.fmt_sel_order_batches, n = o.batches),
                    None => l.sel_no_order.to_string(),
                };
                lines.push((order, Color::srgb(0.75, 0.78, 0.82)));
                if state == crate::production::MachineState::Working {
                    lines.push((
                        crate::tfmt!(l.fmt_sel_fab_progress, p = (f.progress * 100.0) as u32),
                        Color::srgb(0.55, 1.0, 0.65),
                    ));
                }
                if !power.ok() {
                    lines.push((
                        crate::tfmt!(
                            l.fmt_sel_power_lost,
                            status = loc::power_status_label(*power, l)
                        ),
                        Color::srgb(1.0, 0.5, 0.4),
                    ));
                }
                if !power.ok() {
                    lines.push((
                        crate::tfmt!(
                            l.fmt_sel_power_lost,
                            status = loc::power_status_label(*power, l)
                        ),
                        Color::srgb(1.0, 0.5, 0.4),
                    ));
                }
                if demo.is_some() {
                    lines.push((l.sel_marked_demo.into(), Color::srgb(1.0, 0.7, 0.25)));
                }
                lines.push((l.sel_fab_recipe.to_string(), Color::srgb(0.6, 0.66, 0.72)));
            }
        }
        None => {
            lines.push((l.sel_none_a.to_string(), Color::srgb(0.6, 0.66, 0.72)));
            lines.push((l.sel_none_b.to_string(), Color::srgb(0.6, 0.66, 0.72)));
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
    if *last_sig != sig || *last_lang != *lang {
        *last_lang = *lang;
        *last_sig = sig.clone();
        let cfgs: Vec<BtnCfg> = match &sig {
            SelSig::None => Vec::new(),
            SelSig::Crew { .. } => {
                vec![BtnCfg::new(l.work_open_btn, Action::ToggleWorkTab)]
            }
            SelSig::Item { e, marked } => vec![BtnCfg::new(
                if *marked {
                    format!("{} [T]", l.btn_unmark_haul)
                } else {
                    format!("{} [T]", l.btn_mark_haul)
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
                            if allowed[k.index()] {
                                l.btn_allow
                            } else {
                                l.btn_deny
                            },
                            loc::item_label(k, l)
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
                        l.btn_cancel_demo,
                        Action::UnmarkDeconstruct { building: *e },
                    ));
                } else {
                    v.push(BtnCfg::new(
                        l.btn_deconstruct,
                        Action::MarkDeconstruct { building: *e },
                    ));
                }
                v
            }
            SelSig::Generator { e, on, demo, .. } => {
                let mut v = vec![BtnCfg::new(
                    if *on {
                        l.btn_reactor_standby
                    } else {
                        l.btn_reactor_on
                    },
                    Action::SetGeneratorOn { gen: *e, on: !*on },
                )
                .active(*on)];
                if *demo {
                    v.push(BtnCfg::new(
                        l.btn_cancel_demo,
                        Action::UnmarkDeconstruct { building: *e },
                    ));
                } else {
                    v.push(BtnCfg::new(
                        l.btn_deconstruct,
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
                        BtnCfg::new(
                            loc::door_mode_label(m, l),
                            Action::SetDoorMode { door: *e, mode: m },
                        )
                        .active(m == current)
                    })
                    .collect();
                if *demo {
                    v.push(BtnCfg::new(
                        l.btn_cancel_demo,
                        Action::UnmarkDeconstruct { building: *e },
                    ));
                } else {
                    v.push(BtnCfg::new(
                        l.btn_deconstruct,
                        Action::MarkDeconstruct { building: *e },
                    ));
                }
                v
            }
            SelSig::Vent {
                e,
                mode,
                open,
                demo,
            } => {
                let current = match mode {
                    0 => crate::ventilation::VentMode::Supply,
                    1 => crate::ventilation::VentMode::Exhaust,
                    _ => crate::ventilation::VentMode::Balanced,
                };
                let mut v: Vec<BtnCfg> = crate::ventilation::VentMode::ALL
                    .iter()
                    .map(|&m| {
                        BtnCfg::new(
                            loc::vent_mode_label(m, l),
                            Action::SetVentMode { vent: *e, mode: m },
                        )
                        .active(m == current)
                    })
                    .collect();
                v.push(BtnCfg::new(
                    if *open {
                        l.btn_close_vent
                    } else {
                        l.btn_open_vent
                    },
                    Action::SetVentOpen {
                        vent: *e,
                        open: !*open,
                    },
                ));
                if *demo {
                    v.push(BtnCfg::new(
                        l.btn_cancel_demo,
                        Action::UnmarkDeconstruct { building: *e },
                    ));
                } else {
                    v.push(BtnCfg::new(
                        l.btn_deconstruct,
                        Action::MarkDeconstruct { building: *e },
                    ));
                }
                v
            }
            SelSig::Blower { e, dir, on, demo } => {
                let current = match dir {
                    0 => crate::ventilation::Dir4::East,
                    1 => crate::ventilation::Dir4::West,
                    2 => crate::ventilation::Dir4::South,
                    _ => crate::ventilation::Dir4::North,
                };
                let mut v: Vec<BtnCfg> = crate::ventilation::Dir4::ALL
                    .iter()
                    .map(|&d| {
                        BtnCfg::new(
                            loc::dir4_label(d, l),
                            Action::SetBlowerDir { blower: *e, dir: d },
                        )
                        .active(d == current)
                    })
                    .collect();
                v.push(BtnCfg::new(
                    if *on { l.btn_stop } else { l.btn_run },
                    Action::SetBlowerOn {
                        blower: *e,
                        on: !*on,
                    },
                ));
                if *demo {
                    v.push(BtnCfg::new(
                        l.btn_cancel_demo,
                        Action::UnmarkDeconstruct { building: *e },
                    ));
                } else {
                    v.push(BtnCfg::new(
                        l.btn_deconstruct,
                        Action::MarkDeconstruct { building: *e },
                    ));
                }
                v
            }
            SelSig::Tank { e, valve, demo } => {
                let mut v = vec![BtnCfg::new(
                    if *valve {
                        format!("{}: {}", l.valve_label, l.btn_close)
                    } else {
                        format!("{}: {}", l.valve_label, l.btn_open)
                    },
                    Action::SetTankValve {
                        tank: *e,
                        open: !*valve,
                    },
                )];
                if *demo {
                    v.push(BtnCfg::new(
                        l.btn_cancel_demo,
                        Action::UnmarkDeconstruct { building: *e },
                    ));
                } else {
                    v.push(BtnCfg::new(
                        l.btn_deconstruct,
                        Action::MarkDeconstruct { building: *e },
                    ));
                }
                v
            }
            SelSig::Blueprint { e } => vec![BtnCfg::new(
                l.btn_cancel_blueprint,
                Action::CancelBlueprint { blueprint: *e },
            )],
            SelSig::Building { e, demo, .. } => {
                if *demo {
                    vec![BtnCfg::new(
                        l.btn_cancel_demo,
                        Action::UnmarkDeconstruct { building: *e },
                    )]
                } else {
                    vec![BtnCfg::new(
                        l.btn_deconstruct,
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
                        l.btn_plus1,
                        Action::FabAddOrder {
                            fab: *e,
                            batches: 1,
                        },
                    ),
                    BtnCfg::new(
                        l.btn_plus5,
                        Action::FabAddOrder {
                            fab: *e,
                            batches: 5,
                        },
                    ),
                    BtnCfg::new(
                        if *repeat {
                            l.btn_repeat_on
                        } else {
                            l.btn_repeat_off
                        },
                        Action::FabRepeat { fab: *e },
                    )
                    .active(*repeat),
                ];
                if *ordered {
                    v.push(BtnCfg::new(
                        l.btn_clear_order,
                        Action::FabClearOrder { fab: *e },
                    ));
                }
                if *demo {
                    v.push(BtnCfg::new(
                        l.btn_cancel_demo,
                        Action::UnmarkDeconstruct { building: *e },
                    ));
                } else {
                    v.push(BtnCfg::new(
                        l.btn_deconstruct,
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
    lang: Res<Lang>,
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
    let l = strings(*lang);
    if let Ok((mut text, _, _)) = texts.get_mut(hud.tool_hint) {
        let want = match build_mode.0 {
            Some(Tool::Build(kind)) => crate::tfmt!(
                l.fmt_hint_place,
                kind = loc::building_label(kind, l),
                parts = format!("{:?}", crate::building::def(kind).cost)
            ),
            Some(Tool::Deconstruct) => l.hint_deconstruct.to_string(),
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
            crate::tfmt!(
                l.fmt_hud_ship_time,
                time = crate::simtime::format_sim_stamp(clock.now())
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
        let want = crate::tfmt!(
            l.fmt_hud_stats,
            marked = marked,
            stored = stored,
            cap = cap,
            parts = stats.produced,
            built = stats.built,
            idle = idle
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
            let mut line = crate::tfmt!(
                l.fmt_chip,
                arrow = if mov.path.is_empty() { "·" } else { ">" },
                name = crew.name,
                task = task_label(task, &items, &racks, l)
            );
            line.push_str(&crate::tfmt!(
                l.fmt_chip_counts,
                h = crew.delivered,
                b = crew.built,
                o = crew.operated
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
