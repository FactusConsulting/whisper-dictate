use super::test_support::test_app;
use super::*;

#[test]
fn worker_command_passes_wayland_keyboard_layout() {
    let settings = AppSettings {
        xkb_layout: "dk".to_owned(),
        ..Default::default()
    };
    let app = test_app(settings);

    let command = app.worker_command();
    assert_eq!(
        command
            .env
            .iter()
            .find(|(key, _)| key == XKB_LAYOUT_ENV)
            .map(|(_, value)| value.as_str()),
        Some("dk")
    );
}

#[test]
fn configured_keyboard_layout_beats_gnome_detection() {
    let settings = AppSettings {
        xkb_layout: " no ".to_owned(),
        ..Default::default()
    };

    assert_eq!(effective_xkb_layout(&settings).as_deref(), Some("no"));
}

#[test]
fn keyboard_layout_accepts_language_aliases_but_not_en() {
    assert_eq!(normalize_xkb_layout("da").as_deref(), Some("dk"));
    assert_eq!(normalize_xkb_layout("nb").as_deref(), Some("no"));
    assert_eq!(normalize_xkb_layout("uk").as_deref(), Some("ua"));
    assert_eq!(normalize_xkb_layout("en"), None);
}

#[test]
fn parses_gnome_danish_keyboard_layout() {
    assert_eq!(
        parse_gnome_xkb_sources("[('xkb', 'dk')]").as_deref(),
        Some("dk")
    );
    assert_eq!(
        parse_gnome_xkb_sources("[('ibus', 'mozc-jp'), ('xkb', 'dk')]").as_deref(),
        Some("dk")
    );
}

#[test]
fn gnome_keyboard_layout_parser_ignores_us_fallback() {
    assert_eq!(parse_gnome_xkb_sources("[('xkb', 'us')]"), None);
    assert_eq!(
        parse_gnome_xkb_sources("[('xkb', 'us'), ('xkb', 'dk')]").as_deref(),
        Some("dk")
    );
}
