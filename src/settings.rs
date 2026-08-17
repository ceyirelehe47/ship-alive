//! Settings page (Slice 8): language selection + persistence, the CJK UI
//! font, and the static-label refresh machinery.
//!
//! - `Lang` is resolved once at plugin build: `SLICE8_LANG` override →
//!   `settings.ini` next to the exe → LANG/LC_ALL locale hint → English.
//!   Switching (`Action::SetLang`, fired by the settings panel) rewrites the
//!   resource and saves the file immediately.
//! - `UiFont` loads a system CJK font (Microsoft YaHei / SimHei / Noto CJK /
//!   PingFang) so Chinese renders without bundling a font; when found it is
//!   applied to EVERY text (CJK fonts carry Latin glyphs too), otherwise the
//!   engine default stays and Chinese falls back to tofu boxes (logged).
//! - `StaticLabel(fn(&Strings) -> &'static str)` marks texts whose content
//!   only depends on the language (button captions, headers, hints); one
//!   system keeps them in sync, so switching languages re-renders the whole
//!   chrome without restarting.

use crate::jobs::Action;
use crate::loc::{strings, Lang, Strings};
use crate::ui::OnPress;
use bevy::prelude::*;

/// Whether the settings panel is shown ([O] / top-bar button).
#[derive(Resource, Default)]
pub struct SettingsVisible(pub bool);

/// Marks the settings panel root (visibility driver).
#[derive(Component)]
pub struct SettingsRoot;

/// Every button owned by the settings panel.
#[derive(Component, Clone, Copy)]
pub enum SettingsButton {
    Lang(Lang),
    Close,
}

/// A text whose content only depends on the language. Spawn with the
/// current language's string; the sync system rewrites it on switch.
/// Boxed so call sites can capture data (e.g. the building kind).
#[derive(Component)]
pub struct StaticLabel(pub Box<dyn Fn(&Strings) -> &'static str + Send + Sync>);

impl StaticLabel {
    pub fn new(f: impl Fn(&Strings) -> &'static str + Send + Sync + 'static) -> Self {
        Self(Box::new(f))
    }
}

// ---- persistence -----------------------------------------------------------

/// Resolve the settings file path: `settings.ini` next to the executable,
/// falling back to the working directory.
pub fn settings_path() -> std::path::PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let base = exe_dir.unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("settings.ini")
}

/// Pure file → Lang for tests. Missing/corrupt file falls back to `default`.
pub fn load_lang_from(path: &std::path::Path, default: Lang) -> Lang {
    let Ok(text) = std::fs::read_to_string(path) else {
        return default;
    };
    for line in text.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("lang") {
            let value = value.trim_start_matches(['=', ' ', '\t']).trim();
            if let Some(lang) = Lang::from_code(value) {
                return lang;
            }
        }
    }
    default
}

/// Pure Lang → file for tests.
pub fn save_lang_to(path: &std::path::Path, lang: Lang) -> std::io::Result<()> {
    std::fs::write(path, format!("lang={}\n", lang.code()))
}

/// Locale hint: a LANG/LC_ALL containing "zh" picks Chinese.
pub fn detect_lang() -> Lang {
    for key in ["SLICE8_LANG", "LANG", "LC_ALL"] {
        if let Ok(v) = std::env::var(key) {
            if v.is_empty() {
                continue;
            }
            if let Some(lang) = Lang::from_code(&v) {
                return lang;
            }
            if v.to_ascii_lowercase().contains("zh") {
                return Lang::Zh;
            }
        }
    }
    Lang::En
}

// ---- CJK font ---------------------------------------------------------------

/// A system CJK font applied to all UI text (Latin included). `None` keeps
/// the engine default — English still renders, Chinese becomes tofu boxes.
#[derive(Resource)]
pub struct UiFont(pub Option<Handle<Font>>);

impl FromWorld for UiFont {
    fn from_world(world: &mut World) -> Self {
        let candidates: &[&str] = &[
            // Windows: YaHei first (nicest), SimHei as the plain-TTF safety.
            r"C:\Windows\Fonts\msyh.ttc",
            r"C:\Windows\Fonts\simhei.ttf",
            r"C:\Windows\Fonts\simsun.ttc",
            // Linux.
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
            // macOS.
            "/System/Library/Fonts/PingFang.ttc",
        ];
        let mut fonts = world.resource_mut::<Assets<Font>>();
        for path in candidates {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            match Font::try_from_bytes(bytes) {
                Ok(font) => {
                    let handle = fonts.add(font);
                    println!("UIFONT loaded CJK font from {path}");
                    return Self(Some(handle));
                }
                Err(e) => println!("UIFONT {path} failed to parse: {e:?}"),
            }
        }
        println!("UIFONT no system CJK font found — Chinese text will render as boxes");
        Self(None)
    }
}

/// Apply the CJK font to every text as it appears (CJK fonts include Latin
/// glyphs, so both languages render from one font).
fn font_apply_system(ui_font: Res<UiFont>, mut q: Query<&mut TextFont>) {
    let Some(handle) = &ui_font.0 else {
        return;
    };
    for mut tf in q.iter_mut() {
        if &tf.font != handle {
            tf.font = handle.clone_weak();
        }
    }
}

// ---- static labels ----------------------------------------------------------

/// Keep every `StaticLabel` in sync with the active language.
fn static_label_system(lang: Res<Lang>, mut q: Query<(&StaticLabel, &mut Text)>) {
    let l = strings(*lang);
    for (sel, mut text) in q.iter_mut() {
        let want = (sel.0)(l);
        if text.0 != want {
            text.0 = want.to_string();
        }
    }
}

// ---- language switching -----------------------------------------------------

/// Consume `SetLang` actions: flip the resource, persist, log.
fn settings_action_system(
    mut events: EventReader<Action>,
    mut lang: ResMut<Lang>,
    mut log: ResMut<crate::log::EventLog>,
    clock: Res<crate::simtime::SimClock>,
    mut visible: ResMut<SettingsVisible>,
    mut vis_q: Query<&mut Visibility, With<SettingsRoot>>,
) {
    let mut switched = false;
    for action in events.read() {
        match *action {
            Action::SetLang { to } => {
                if *lang != to {
                    *lang = to;
                    switched = true;
                }
            }
            Action::ToggleSettings => {
                visible.0 = !visible.0;
            }
            _ => {}
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
    if switched {
        let l = strings(*lang);
        if let Err(e) = save_lang_to(&settings_path(), *lang) {
            println!("SETTINGS failed to save settings.ini: {e:?}");
        }
        log.push(
            clock.now(),
            crate::log::LogKind::Info,
            crate::tfmt!(l.fmt_log_lang, name = (*lang).native_name()),
        );
    }
}

// ---- panel -------------------------------------------------------------------

/// Marker-styled buttons + highlight for the language options.
fn settings_panel_system(
    visible: Res<SettingsVisible>,
    lang: Res<Lang>,
    mut btns: Query<(&SettingsButton, &Interaction, &mut BackgroundColor)>,
) {
    if !visible.0 {
        return;
    }
    for (btn, interaction, mut bg) in btns.iter_mut() {
        let hovered = *interaction == Interaction::Hovered || *interaction == Interaction::Pressed;
        let want = match btn {
            SettingsButton::Lang(l) => {
                let active = *l == *lang;
                if active {
                    crate::ui::BUTTON_ACTIVE
                } else if hovered {
                    crate::ui::BUTTON_HOVER
                } else {
                    crate::ui::PANEL_BG
                }
            }
            SettingsButton::Close => {
                if hovered {
                    crate::ui::BUTTON_HOVER
                } else {
                    crate::ui::PANEL_BG
                }
            }
        };
        if bg.0 != want {
            bg.0 = want;
        }
    }
}

fn slabel(
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

fn build_settings_panel(mut commands: Commands, lang: Res<Lang>) {
    commands
        .spawn((
            SettingsRoot,
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
                panel
                    .spawn(Node {
                        flex_direction: FlexDirection::Row,
                        column_gap: Val::Px(6.0),
                        align_items: AlignItems::Center,
                        ..default()
                    })
                    .with_children(|row| {
                        slabel(
                            row,
                            *lang,
                            |s| s.set_title,
                            15.0,
                            Color::srgb(0.95, 0.85, 0.55),
                        );
                        row.spawn(Node {
                            flex_grow: 1.0,
                            ..default()
                        });
                        row.spawn((
                            Button,
                            Interaction::default(),
                            OnPress(Action::ToggleSettings),
                            SettingsButton::Close,
                            Node {
                                height: Val::Px(24.0),
                                padding: UiRect::horizontal(Val::Px(10.0)),
                                margin: UiRect::all(Val::Px(2.0)),
                                align_items: AlignItems::Center,
                                justify_content: JustifyContent::Center,
                                ..default()
                            },
                            BackgroundColor(crate::ui::PANEL_BG),
                        ))
                        .with_children(|b| {
                            slabel(b, *lang, |s| s.set_close, 12.0, Color::WHITE);
                        });
                    });

                slabel(
                    panel,
                    *lang,
                    |s| s.set_language,
                    12.0,
                    Color::srgb(0.55, 0.62, 0.7),
                );
                panel
                    .spawn(Node {
                        flex_direction: FlexDirection::Row,
                        column_gap: Val::Px(6.0),
                        ..default()
                    })
                    .with_children(|row| {
                        for l in [Lang::En, Lang::Zh] {
                            row.spawn((
                                Button,
                                Interaction::default(),
                                OnPress(Action::SetLang { to: l }),
                                SettingsButton::Lang(l),
                                Node {
                                    width: Val::Px(110.0),
                                    height: Val::Px(26.0),
                                    margin: UiRect::all(Val::Px(2.0)),
                                    align_items: AlignItems::Center,
                                    justify_content: JustifyContent::Center,
                                    ..default()
                                },
                                BackgroundColor(crate::ui::PANEL_BG),
                            ))
                            .with_children(|b| {
                                let e = b
                                    .spawn((
                                        Text::new(l.native_name()),
                                        TextFont {
                                            font_size: 13.0,
                                            ..default()
                                        },
                                        TextColor(Color::WHITE),
                                    ))
                                    .id();
                                let _ = e;
                            });
                        }
                    });
                slabel(
                    panel,
                    *lang,
                    |s| s.set_note,
                    10.0,
                    Color::srgb(0.5, 0.55, 0.62),
                );
            });
        });
}

pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        // Resolved once, before any Startup system (both render.rs room
        // labels and the UI read it at spawn time): explicit SLICE8_LANG
        // override → settings.ini → locale hint (LANG/LC_ALL) → English.
        let path = settings_path();
        let lang = if std::env::var("SLICE8_LANG").is_ok() {
            detect_lang()
        } else if path.exists() {
            load_lang_from(&path, detect_lang())
        } else {
            detect_lang()
        };
        app.insert_resource(lang);
        let mut visible = SettingsVisible(false);
        if std::env::var("SLICE8_VIEW").as_deref() == Ok("settings") {
            visible.0 = true;
        }
        app.insert_resource(visible);
        app.init_resource::<UiFont>();
        // Spawn after the other overlay roots so the panel stacks on top.
        app.add_systems(
            Startup,
            build_settings_panel
                .after(crate::worktab::build_work_tab)
                .after(crate::ui_overlay::build_overlay),
        );
        app.add_systems(
            Update,
            (
                settings_action_system,
                font_apply_system,
                static_label_system,
                settings_panel_system,
            ),
        );
    }
}
