use super::*;

fn parse_json(line: &str) -> serde_json::Value {
    serde_json::from_str(line).unwrap_or_else(|e| panic!("bad JSON {line:?}: {e}"))
}

#[test]
fn plain_install_line_has_driver_and_chord() {
    let line = format_plain(&CaptureEvent::ListenerInstalled {
        driver: "rdev",
        chord: "ctrl_l+shift_l+l".to_owned(),
    });
    assert!(line.starts_with(OUTPUT_PREFIX), "prefix: {line}");
    assert!(line.contains("driver=rdev"));
    assert!(line.contains("chord=ctrl_l+shift_l+l"));
}

#[path = "parsing_tests.rs"]
mod parsing;

#[path = "output_tests.rs"]
mod output;
