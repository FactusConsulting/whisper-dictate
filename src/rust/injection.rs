use std::process::Command;

use anyhow::{anyhow, Result};

const WAYLAND_MODIFIER_RELEASES: &[&str] = &[
    "29:0",  // KEY_LEFTCTRL
    "97:0",  // KEY_RIGHTCTRL
    "42:0",  // KEY_LEFTSHIFT
    "54:0",  // KEY_RIGHTSHIFT
    "56:0",  // KEY_LEFTALT
    "100:0", // KEY_RIGHTALT
    "125:0", // KEY_LEFTMETA
    "126:0", // KEY_RIGHTMETA
];
const WAYLAND_CTRL_V: &[&str] = &["29:1", "47:1", "47:0", "29:0"];
const WAYLAND_CTRL_SHIFT_V: &[&str] = &["29:1", "42:1", "47:1", "47:0", "42:0", "29:0"];
const LINUX_TERMINAL_TARGETS: &[&str] = &[
    "terminal",
    "ptyxis",
    "kgx",
    "konsole",
    "xterm",
    "alacritty",
    "wezterm",
    "ghostty",
    "kitty",
    "tilix",
    "gnome-console",
    "gnome-terminal",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YdotoolOp {
    Type(String),
    Key(Vec<String>),
}

pub fn handle_inject_text(
    mode: &str,
    text: &str,
    xkb_layout: &str,
    target_title: &str,
    target_process: &str,
) -> Result<()> {
    match mode {
        "type" => type_text(text, xkb_layout),
        "paste" => paste_shortcut(target_title, target_process),
        other => Err(anyhow!("unsupported inject-text mode: {other}")),
    }
}

pub fn build_ydotool_ops(text: &str, xkb_layout: &str) -> Vec<YdotoolOp> {
    let mut ops = Vec::new();
    let mut buffer = String::new();
    for ch in text.chars() {
        if let Some(keys) = keycodes_for(xkb_layout, ch) {
            if !buffer.is_empty() {
                ops.push(YdotoolOp::Type(std::mem::take(&mut buffer)));
            }
            ops.push(YdotoolOp::Key(keys));
        } else {
            buffer.push(ch);
        }
    }
    if !buffer.is_empty() {
        ops.push(YdotoolOp::Type(buffer));
    }
    ops
}

pub fn target_prefers_terminal_paste(target_title: &str, target_process: &str) -> bool {
    let target = format!("{target_title} {target_process}").to_lowercase();
    if target.trim().is_empty() {
        return true;
    }
    LINUX_TERMINAL_TARGETS
        .iter()
        .any(|term| target.contains(term))
}

pub fn paste_shortcut_args(target_title: &str, target_process: &str) -> Vec<&'static str> {
    let mut args = Vec::with_capacity(WAYLAND_MODIFIER_RELEASES.len() + WAYLAND_CTRL_SHIFT_V.len());
    args.extend_from_slice(WAYLAND_MODIFIER_RELEASES);
    if target_prefers_terminal_paste(target_title, target_process) {
        args.extend_from_slice(WAYLAND_CTRL_SHIFT_V);
    } else {
        args.extend_from_slice(WAYLAND_CTRL_V);
    }
    args
}

fn type_text(text: &str, xkb_layout: &str) -> Result<()> {
    for op in build_ydotool_ops(text, xkb_layout) {
        match op {
            YdotoolOp::Type(chunk) => run_ydotool(["type", "--", &chunk])?,
            YdotoolOp::Key(keys) => {
                let mut args = vec!["key".to_owned()];
                args.extend(keys);
                run_ydotool(args.iter().map(String::as_str))?;
            }
        }
    }
    Ok(())
}

fn paste_shortcut(target_title: &str, target_process: &str) -> Result<()> {
    run_ydotool(std::iter::once("key").chain(paste_shortcut_args(target_title, target_process)))
}

fn run_ydotool<'a>(args: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let args: Vec<&str> = args.into_iter().collect();
    let output = Command::new("ydotool").args(&args).output()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(anyhow!(
        "ydotool {} failed: {}",
        args.join(" "),
        stderr.trim()
    ))
}

fn keycodes_for(layout: &str, ch: char) -> Option<Vec<String>> {
    let layout = if layout == "no" { "dk" } else { layout };
    match layout {
        "dk" => dk_keycodes(ch),
        "se" => se_keycodes(ch),
        "de" => de_keycodes(ch),
        "fi" => fi_keycodes(ch),
        "fr" => fr_keycodes(ch),
        "it" => it_keycodes(ch),
        "es" => es_keycodes(ch),
        "pt" => pt_keycodes(ch),
        "br" => br_keycodes(ch),
        "pl" => pl_keycodes(ch),
        "ua" => ua_keycodes(ch),
        _ => None,
    }
}

fn dk_keycodes(ch: char) -> Option<Vec<String>> {
    match ch {
        'å' => Some(key(26)),
        'Å' => Some(shift_key(26)),
        'æ' => Some(key(39)),
        'Æ' => Some(shift_key(39)),
        'ø' => Some(key(40)),
        'Ø' => Some(shift_key(40)),
        _ => nordic_de_punct(ch),
    }
}

fn se_keycodes(ch: char) -> Option<Vec<String>> {
    match ch {
        'å' => Some(key(26)),
        'Å' => Some(shift_key(26)),
        'ä' => Some(key(40)),
        'Ä' => Some(shift_key(40)),
        'ö' => Some(key(39)),
        'Ö' => Some(shift_key(39)),
        _ => nordic_de_punct(ch),
    }
}

fn de_keycodes(ch: char) -> Option<Vec<String>> {
    match ch {
        'ä' => Some(key(40)),
        'Ä' => Some(shift_key(40)),
        'ö' => Some(key(39)),
        'Ö' => Some(shift_key(39)),
        'ü' => Some(key(26)),
        'Ü' => Some(shift_key(26)),
        _ => nordic_de_punct(ch),
    }
}

fn fi_keycodes(ch: char) -> Option<Vec<String>> {
    match ch {
        'ä' => Some(key(40)),
        'Ä' => Some(shift_key(40)),
        'ö' => Some(key(39)),
        'Ö' => Some(shift_key(39)),
        _ => nordic_de_punct(ch),
    }
}

// French AZERTY. Derived from the standard XKB `fr` layout, NOT hardware-tested
// (see the README status table). The dedicated number-row accents (é è ç à) and
// ù have no simple uppercase on AZERTY, so only their lowercase forms are mapped;
// circumflex/diaeresis go through the dead key right of P (KEY_LEFTBRACE=26).
fn fr_keycodes(ch: char) -> Option<Vec<String>> {
    match ch {
        'é' => Some(key(3)),  // KEY_2
        'è' => Some(key(8)),  // KEY_7
        'ç' => Some(key(10)), // KEY_9
        'à' => Some(key(11)), // KEY_0
        'ù' => Some(key(40)), // KEY_APOSTROPHE
        'â' => Some(dead(26, 16)),
        'Â' => Some(dead_up(26, 16)),
        'ê' => Some(dead(26, 18)),
        'Ê' => Some(dead_up(26, 18)),
        'î' => Some(dead(26, 23)),
        'Î' => Some(dead_up(26, 23)),
        'ô' => Some(dead(26, 24)),
        'Ô' => Some(dead_up(26, 24)),
        'û' => Some(dead(26, 22)),
        'Û' => Some(dead_up(26, 22)),
        'ë' => Some(shift_dead(26, 18)),
        'Ë' => Some(shift_dead_up(26, 18)),
        'ï' => Some(shift_dead(26, 23)),
        'Ï' => Some(shift_dead_up(26, 23)),
        'ü' => Some(shift_dead(26, 22)),
        'Ü' => Some(shift_dead_up(26, 22)),
        _ => None,
    }
}

// Italian QWERTY. Derived from the standard XKB `it` layout, NOT hardware-tested.
// The accented vowels sit on dedicated keys to the right of the letters; their
// uppercase forms are not directly reachable on this layout, so only lowercase
// is mapped (uppercase falls back to Unicode `type`).
fn it_keycodes(ch: char) -> Option<Vec<String>> {
    match ch {
        'ì' => Some(key(13)),       // KEY_EQUAL
        'è' => Some(key(26)),       // KEY_LEFTBRACE
        'é' => Some(shift_key(26)), // Shift of the è key
        'ò' => Some(key(39)),       // KEY_SEMICOLON
        'ç' => Some(shift_key(39)), // Shift of the ò key
        'à' => Some(key(40)),       // KEY_APOSTROPHE
        'ù' => Some(key(43)),       // KEY_BACKSLASH (ISO key by Enter)
        _ => None,
    }
}

fn es_keycodes(ch: char) -> Option<Vec<String>> {
    match ch {
        'ñ' => Some(key(39)),
        'Ñ' => Some(shift_key(39)),
        'á' => Some(dead(40, 30)),
        'Á' => Some(dead_up(40, 30)),
        'é' => Some(dead(40, 18)),
        'É' => Some(dead_up(40, 18)),
        'í' => Some(dead(40, 23)),
        'Í' => Some(dead_up(40, 23)),
        'ó' => Some(dead(40, 24)),
        'Ó' => Some(dead_up(40, 24)),
        'ú' => Some(dead(40, 22)),
        'Ú' => Some(dead_up(40, 22)),
        'ü' => Some(shift_dead(40, 22)),
        'Ü' => Some(shift_dead_up(40, 22)),
        _ => None,
    }
}

fn pt_keycodes(ch: char) -> Option<Vec<String>> {
    match ch {
        'ç' => Some(key(39)),
        'Ç' => Some(shift_key(39)),
        'á' => Some(dead(27, 30)),
        'Á' => Some(dead_up(27, 30)),
        'é' => Some(dead(27, 18)),
        'É' => Some(dead_up(27, 18)),
        'í' => Some(dead(27, 23)),
        'Í' => Some(dead_up(27, 23)),
        'ó' => Some(dead(27, 24)),
        'Ó' => Some(dead_up(27, 24)),
        'ú' => Some(dead(27, 22)),
        'Ú' => Some(dead_up(27, 22)),
        'à' => Some(shift_dead(27, 30)),
        'À' => Some(shift_dead_up(27, 30)),
        'ã' => Some(dead(43, 30)),
        'Ã' => Some(dead_up(43, 30)),
        'õ' => Some(dead(43, 24)),
        'Õ' => Some(dead_up(43, 24)),
        'â' => Some(shift_dead(43, 30)),
        'Â' => Some(shift_dead_up(43, 30)),
        'ê' => Some(shift_dead(43, 18)),
        'Ê' => Some(shift_dead_up(43, 18)),
        'ô' => Some(shift_dead(43, 24)),
        'Ô' => Some(shift_dead_up(43, 24)),
        _ => None,
    }
}

fn br_keycodes(ch: char) -> Option<Vec<String>> {
    match ch {
        'ç' => Some(key(39)),
        'Ç' => Some(shift_key(39)),
        'ã' => Some(dead(40, 30)),
        'Ã' => Some(dead_up(40, 30)),
        'õ' => Some(dead(40, 24)),
        'Õ' => Some(dead_up(40, 24)),
        'â' => Some(shift_dead(40, 30)),
        'Â' => Some(shift_dead_up(40, 30)),
        'ê' => Some(shift_dead(40, 18)),
        'Ê' => Some(shift_dead_up(40, 18)),
        'ô' => Some(shift_dead(40, 24)),
        'Ô' => Some(shift_dead_up(40, 24)),
        'á' => Some(dead(39, 30)),
        'Á' => Some(dead_up(39, 30)),
        'é' => Some(dead(39, 18)),
        'É' => Some(dead_up(39, 18)),
        'í' => Some(dead(39, 23)),
        'Í' => Some(dead_up(39, 23)),
        'ó' => Some(dead(39, 24)),
        'Ó' => Some(dead_up(39, 24)),
        'ú' => Some(dead(39, 22)),
        'Ú' => Some(dead_up(39, 22)),
        _ => None,
    }
}

fn pl_keycodes(ch: char) -> Option<Vec<String>> {
    match ch {
        'ą' => Some(altgr(30)),
        'Ą' => Some(altgr_up(30)),
        'ę' => Some(altgr(18)),
        'Ę' => Some(altgr_up(18)),
        'ó' => Some(altgr(24)),
        'Ó' => Some(altgr_up(24)),
        'ś' => Some(altgr(31)),
        'Ś' => Some(altgr_up(31)),
        'ź' => Some(altgr(45)),
        'Ź' => Some(altgr_up(45)),
        'ż' => Some(altgr(44)),
        'Ż' => Some(altgr_up(44)),
        'ć' => Some(altgr(46)),
        'Ć' => Some(altgr_up(46)),
        'ń' => Some(altgr(49)),
        'Ń' => Some(altgr_up(49)),
        'ł' => Some(altgr(38)),
        'Ł' => Some(altgr_up(38)),
        _ => None,
    }
}

fn ua_keycodes(ch: char) -> Option<Vec<String>> {
    match ch {
        'й' => Some(key(16)),
        'Й' => Some(shift_key(16)),
        'ц' => Some(key(17)),
        'Ц' => Some(shift_key(17)),
        'у' => Some(key(18)),
        'У' => Some(shift_key(18)),
        'к' => Some(key(19)),
        'К' => Some(shift_key(19)),
        'е' => Some(key(20)),
        'Е' => Some(shift_key(20)),
        'н' => Some(key(21)),
        'Н' => Some(shift_key(21)),
        'г' => Some(key(22)),
        'Г' => Some(shift_key(22)),
        'ш' => Some(key(23)),
        'Ш' => Some(shift_key(23)),
        'щ' => Some(key(24)),
        'Щ' => Some(shift_key(24)),
        'з' => Some(key(25)),
        'З' => Some(shift_key(25)),
        'х' => Some(key(26)),
        'Х' => Some(shift_key(26)),
        'ї' => Some(key(27)),
        'Ї' => Some(shift_key(27)),
        'ф' => Some(key(30)),
        'Ф' => Some(shift_key(30)),
        'і' => Some(key(31)),
        'І' => Some(shift_key(31)),
        'в' => Some(key(32)),
        'В' => Some(shift_key(32)),
        'а' => Some(key(33)),
        'А' => Some(shift_key(33)),
        'п' => Some(key(34)),
        'П' => Some(shift_key(34)),
        'р' => Some(key(35)),
        'Р' => Some(shift_key(35)),
        'о' => Some(key(36)),
        'О' => Some(shift_key(36)),
        'л' => Some(key(37)),
        'Л' => Some(shift_key(37)),
        'д' => Some(key(38)),
        'Д' => Some(shift_key(38)),
        'ж' => Some(key(39)),
        'Ж' => Some(shift_key(39)),
        'є' => Some(key(40)),
        'Є' => Some(shift_key(40)),
        'ґ' => Some(key(43)),
        'Ґ' => Some(shift_key(43)),
        'я' => Some(key(44)),
        'Я' => Some(shift_key(44)),
        'ч' => Some(key(45)),
        'Ч' => Some(shift_key(45)),
        'с' => Some(key(46)),
        'С' => Some(shift_key(46)),
        'м' => Some(key(47)),
        'М' => Some(shift_key(47)),
        'и' => Some(key(48)),
        'И' => Some(shift_key(48)),
        'т' => Some(key(49)),
        'Т' => Some(shift_key(49)),
        'ь' => Some(key(50)),
        'Ь' => Some(shift_key(50)),
        'б' => Some(key(51)),
        'Б' => Some(shift_key(51)),
        'ю' => Some(key(52)),
        'Ю' => Some(shift_key(52)),
        _ => None,
    }
}

fn nordic_de_punct(ch: char) -> Option<Vec<String>> {
    match ch {
        '?' => Some(shift_key(12)),
        '-' => Some(key(53)),
        '_' => Some(shift_key(53)),
        ':' => Some(shift_key(52)),
        ';' => Some(shift_key(51)),
        '/' => Some(shift_key(8)),
        '"' => Some(shift_key(3)),
        _ => None,
    }
}

fn key(code: u16) -> Vec<String> {
    vec![format!("{code}:1"), format!("{code}:0")]
}

fn shift_key(code: u16) -> Vec<String> {
    vec![
        "42:1".to_owned(),
        format!("{code}:1"),
        format!("{code}:0"),
        "42:0".to_owned(),
    ]
}

fn dead(dead_key: u16, letter: u16) -> Vec<String> {
    let mut keys = key(dead_key);
    keys.extend(key(letter));
    keys
}

fn dead_up(dead_key: u16, letter: u16) -> Vec<String> {
    let mut keys = key(dead_key);
    keys.extend(shift_key(letter));
    keys
}

fn shift_dead(dead_key: u16, letter: u16) -> Vec<String> {
    let mut keys = shift_key(dead_key);
    keys.extend(key(letter));
    keys
}

fn shift_dead_up(dead_key: u16, letter: u16) -> Vec<String> {
    let mut keys = shift_key(dead_key);
    keys.extend(shift_key(letter));
    keys
}

fn altgr(letter: u16) -> Vec<String> {
    vec![
        "100:1".to_owned(),
        format!("{letter}:1"),
        format!("{letter}:0"),
        "100:0".to_owned(),
    ]
}

fn altgr_up(letter: u16) -> Vec<String> {
    vec![
        "100:1".to_owned(),
        "42:1".to_owned(),
        format!("{letter}:1"),
        format!("{letter}:0"),
        "42:0".to_owned(),
        "100:0".to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dk_direct_text_splits_special_chars() {
        assert_eq!(
            build_ydotool_ops("høre", "dk"),
            vec![
                YdotoolOp::Type("h".to_owned()),
                YdotoolOp::Key(key_codes(&["40:1", "40:0"])),
                YdotoolOp::Type("re".to_owned()),
            ]
        );
    }

    #[test]
    fn no_alias_uses_danish_keycodes() {
        assert_eq!(
            build_ydotool_ops("æøå", "no"),
            vec![
                YdotoolOp::Key(key_codes(&["39:1", "39:0"])),
                YdotoolOp::Key(key_codes(&["40:1", "40:0"])),
                YdotoolOp::Key(key_codes(&["26:1", "26:0"])),
            ]
        );
    }

    #[test]
    fn unknown_layout_keeps_unicode_in_type_chunk_for_fallback_behavior() {
        assert_eq!(
            build_ydotool_ops("høre", "us"),
            vec![YdotoolOp::Type("høre".to_owned())]
        );
    }

    #[test]
    fn terminal_or_unknown_target_uses_ctrl_shift_v() {
        assert!(target_prefers_terminal_paste("", ""));
        assert!(target_prefers_terminal_paste(
            "whisper-dictate - Terminal",
            ""
        ));
        assert!(paste_shortcut_args("", "").ends_with(WAYLAND_CTRL_SHIFT_V));
    }

    #[test]
    fn known_text_editor_target_uses_ctrl_v() {
        assert!(!target_prefers_terminal_paste(
            "Text Editor",
            "gnome-text-editor"
        ));
        assert!(paste_shortcut_args("Text Editor", "gnome-text-editor").ends_with(WAYLAND_CTRL_V));
    }

    #[test]
    fn supported_layouts_cover_expected_non_ascii_characters() {
        for (layout, chars) in [
            ("dk", "æøåÆØÅ"),
            ("no", "æøåÆØÅ"),
            ("se", "äöåÄÖÅ"),
            ("de", "äöüÄÖÜ"),
            ("fi", "äöÄÖ"),
            ("fr", "éèçàùâêîôûëïüÂÊÎÔÛËÏÜ"),
            ("it", "ìèéòçàù"),
            ("es", "ñÑáéíóúÁÉÍÓÚüÜ"),
            ("pt", "çÇáéíóúÁÉÍÓÚàÀãõÃÕâêôÂÊÔ"),
            ("br", "çÇãõÃÕâêôÂÊÔáéíóúÁÉÍÓÚ"),
            ("pl", "ąęóśźżćńłĄĘÓŚŹŻĆŃŁ"),
            (
                "ua",
                "йцукенгшщзхїфівапролджєґячсмитьбюЙЦУКЕНГШЩЗХЇФІВАПРОЛДЖЄҐЯЧСМИТЬБЮ",
            ),
        ] {
            for ch in chars.chars() {
                assert!(
                    keycodes_for(layout, ch).is_some(),
                    "{layout} missing keycodes for {ch:?}"
                );
            }
        }
    }

    #[test]
    fn nordic_punctuation_is_available_for_nordic_and_german_layouts() {
        for layout in ["dk", "no", "se", "de", "fi"] {
            for ch in "?-_:;/\"".chars() {
                assert!(
                    keycodes_for(layout, ch).is_some(),
                    "{layout} missing keycodes for {ch:?}"
                );
            }
        }
    }

    #[test]
    fn generated_keycodes_are_balanced_per_key() {
        for (layout, chars) in [
            ("dk", "æøåÆØÅ?-_:;/\""),
            ("no", "æøåÆØÅ?-_:;/\""),
            ("se", "äöåÄÖÅ?-_:;/\""),
            ("de", "äöüÄÖÜ?-_:;/\""),
            ("fi", "äöÄÖ?-_:;/\""),
            ("fr", "éèçàùâêîôûëïüÂÊÎÔÛËÏÜ"),
            ("it", "ìèéòçàù"),
            ("es", "ñÑáéíóúÁÉÍÓÚüÜ"),
            ("pt", "çÇáéíóúÁÉÍÓÚàÀãõÃÕâêôÂÊÔ"),
            ("br", "çÇãõÃÕâêôÂÊÔáéíóúÁÉÍÓÚ"),
            ("pl", "ąęóśźżćńłĄĘÓŚŹŻĆŃŁ"),
            (
                "ua",
                "йцукенгшщзхїфівапролджєґячсмитьбюЙЦУКЕНГШЩЗХЇФІВАПРОЛДЖЄҐЯЧСМИТЬБЮ",
            ),
        ] {
            for ch in chars.chars() {
                let codes = keycodes_for(layout, ch).unwrap();
                assert_balanced(layout, ch, &codes);
            }
        }
    }

    fn key_codes(codes: &[&str]) -> Vec<String> {
        codes.iter().map(|code| (*code).to_owned()).collect()
    }

    fn assert_balanced(layout: &str, ch: char, codes: &[String]) {
        let mut balance = std::collections::BTreeMap::<&str, i32>::new();
        for code in codes {
            let (key, state) = code
                .split_once(':')
                .unwrap_or_else(|| panic!("invalid keycode token {code:?} for {layout}/{ch}"));
            match state {
                "1" => *balance.entry(key).or_default() += 1,
                "0" => *balance.entry(key).or_default() -= 1,
                _ => panic!("invalid keycode state {code:?} for {layout}/{ch}"),
            }
        }
        for (key, net) in balance {
            assert_eq!(
                net, 0,
                "keycode {key} is unbalanced for {layout}/{ch}: {codes:?}"
            );
        }
    }
}
