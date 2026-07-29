use super::terminal_run::{plan_terminal_run, TerminalRunPlan};

#[test]
fn empty_engine_value_uses_native_rust_plan() {
    let plan = plan_terminal_run(Vec::new(), Some("   "))
        .expect("an empty engine selector should use the native default");

    assert!(
        matches!(plan, TerminalRunPlan::Rust(_)),
        "empty VOICEPI_DICTATE_ENGINE must not construct a Python plan"
    );
}

#[test]
fn native_help_short_circuits_before_runtime_construction() {
    for flag in ["--help", "-h"] {
        let plan = plan_terminal_run(vec![flag.to_owned()], None)
            .expect("native run help should parse without starting a runtime");

        assert!(
            matches!(plan, TerminalRunPlan::Help),
            "{flag} must select the help plan"
        );
    }
}

#[test]
fn autodetect_wins_over_language_in_both_argument_orders() {
    for args in [
        vec![
            "--lang".to_owned(),
            "da".to_owned(),
            "--autodetect".to_owned(),
        ],
        vec![
            "--autodetect".to_owned(),
            "--lang".to_owned(),
            "da".to_owned(),
        ],
    ] {
        let plan = plan_terminal_run(args, None).expect("native flags should parse");
        let TerminalRunPlan::Rust(args) = plan else {
            panic!("default engine must produce a native plan");
        };

        assert!(
            args.env_overrides
                .iter()
                .any(|(name, value)| name == "VOICEPI_LANG" && value.is_empty()),
            "--autodetect must clear the language hint"
        );
        assert!(
            !args
                .env_overrides
                .iter()
                .any(|(name, value)| name == "VOICEPI_LANG" && value == "da"),
            "--lang must not override --autodetect"
        );
    }
}
