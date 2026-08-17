//! Slice 8 integration tests: settings persistence, language resolution
//! from files, and the CJK font load path (skipped when the OS has none).

use bevy::prelude::Font;
use ship_alive::loc::Lang;
use ship_alive::settings::{load_lang_from, save_lang_to};

fn temp_path(tag: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "ship_alive_i18n_{tag}_{}.ini",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    p
}

#[test]
fn settings_round_trip_preserves_both_languages() {
    for lang in [Lang::En, Lang::Zh] {
        let path = temp_path("round");
        save_lang_to(&path, lang).expect("save");
        assert_eq!(load_lang_from(&path, Lang::En), lang, "file must win");
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn missing_or_corrupt_settings_file_falls_back_to_default() {
    let path = temp_path("missing");
    assert_eq!(load_lang_from(&path, Lang::En), Lang::En);
    assert_eq!(load_lang_from(&path, Lang::Zh), Lang::Zh);

    std::fs::write(&path, "garbage without a lang key\n").unwrap();
    assert_eq!(load_lang_from(&path, Lang::En), Lang::En);

    std::fs::write(&path, "lang=??\n").unwrap();
    assert_eq!(load_lang_from(&path, Lang::Zh), Lang::Zh);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn settings_file_format_is_stable() {
    let path = temp_path("format");
    save_lang_to(&path, Lang::Zh).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text, "lang=zh\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn system_cjk_font_parses_when_present() {
    // The game loads a system CJK font so Chinese renders without bundling
    // one. This verifies the parse path on machines that have a candidate
    // font (Windows always ships Microsoft YaHei); elsewhere it skips.
    let candidates = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/System/Library/Fonts/PingFang.ttc",
    ];
    let mut tried = 0;
    for path in candidates {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        tried += 1;
        assert!(
            Font::try_from_bytes(bytes).is_ok(),
            "system font {path} must parse"
        );
    }
    if tried == 0 {
        eprintln!("no system CJK font present; skipping parse check");
    }
}

#[test]
fn localized_strings_cover_the_key_surfaces() {
    // Anchor the coverage claim: the two languages differ on chrome drawn
    // from every major module family (hud / sidebar / tooltips / logs /
    // building labels / room annotations).
    let en = ship_alive::loc::strings(Lang::En);
    let zh = ship_alive::loc::strings(Lang::Zh);
    let pairs = [
        (en.hud_ship_status, zh.hud_ship_status),
        (en.env_ventilation, zh.env_ventilation),
        (en.fmt_tip_gas, zh.fmt_tip_gas),
        (en.fmt_log_claimed, zh.fmt_log_claimed),
        (en.b_fabricator, zh.b_fabricator),
        (en.room_cargo, zh.room_cargo),
        (en.work_hint, zh.work_hint),
        (en.set_note, zh.set_note),
    ];
    for (e, z) in pairs {
        assert_ne!(e, z, "untranslated pair: {e:?}");
    }
}
