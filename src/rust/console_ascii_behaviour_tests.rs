//! Behaviour tests for the console ASCII guard in [`super`].
//!
//! Split from `console_ascii_tests.rs` to keep both files under the repo's
//! ~500-line ceiling (AGENTS.md). Every case here is built from a construct
//! that defeated an earlier version of the guard, so the file reads as the
//! bug history: raw-string prefixes and hash counts, `'"'` char literals,
//! nested block comments, `\u{2014}` escapes, CRLF line continuations,
//! array-vs-attribute confusion, and `#[cfg(test)]` in each position it
//! occurs in this codebase.

use super::*;

fn scan(src: &str, rel: &str) -> Vec<String> {
    let stream = normalize_line_endings(src)
        .parse::<TokenStream>()
        .expect("tokenizes");
    let mut out = Vec::new();
    scan_tokens(
        strip_cfg_test(stream),
        rel,
        is_ui_scope(rel),
        false,
        &mut out,
    );
    out.into_iter().map(|v| v.to_string()).collect()
}

#[test]
fn plain_violation_is_reported() {
    assert_eq!(scan(r#"fn f() { println!("a — b"); }"#, "x.rs").len(), 1);
}

#[test]
fn clean_source_is_silent() {
    assert!(scan(r#"fn f() { println!("a - b"); }"#, "x.rs").is_empty());
}

#[test]
fn unicode_escape_is_decoded_before_checking() {
    // The source contains no em dash, but the program prints one. A
    // source-text scan misses this entirely; `ui/app.rs` already uses
    // `\u{2026}` in production, so the syntax is in real use here.
    let hits = scan(r#"fn f() { println!("bad \u{2014} output"); }"#, "x.rs");
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn char_literal_value_is_checked_not_just_skipped() {
    // Skipping char literals to avoid the `'"'` desync must not mean
    // ignoring what they contain.
    assert_eq!(scan(r#"fn f() { println!("{}", '—'); }"#, "x.rs").len(), 1);
    assert_eq!(
        scan(r#"fn f() { println!("{}", '\u{2014}'); }"#, "x.rs").len(),
        1
    );
}

#[test]
fn quote_char_literal_does_not_hide_the_next_string() {
    // `'"'` is real code here (keymap.rs, runtime/mod.rs). A hand-rolled
    // scanner read it as a string opener and desynced for the rest of the
    // file.
    let src = "fn f() { let _q = '\"'; println!(\"bad — output\"); }";
    assert_eq!(scan(src, "x.rs").len(), 1);
}

#[test]
fn raw_strings_of_every_prefix_and_hash_count_are_atomic() {
    // `r##"has "# inside"##` ends only at a quote plus the SAME hash
    // count; `br"..."` and `cr#"..."#` are distinct prefixes. Getting any
    // of these wrong pairs quotes across expressions and hides what
    // follows.
    for opener in [
        r####"let _ = r##"has "# inside"##;"####,
        r####"let _ = br"bytes";"####,
        r####"let _ = cr#"c "string""#;"####,
    ] {
        let src = format!("fn f() {{ {opener} println!(\"bad — output\"); }}");
        assert_eq!(scan(&src, "x.rs").len(), 1, "hidden by: {opener}");
    }
}

#[test]
fn nested_block_comment_does_not_hide_the_next_string() {
    // Rust block comments nest; stopping at the first `*/` leaves the
    // outer comment's text being parsed as code.
    let src = "fn f() { /* outer /* inner */ \" note */ println!(\"bad — output\"); }";
    assert_eq!(scan(src, "x.rs").len(), 1);
}

#[test]
fn array_literal_is_scanned_but_attribute_body_is_not() {
    // The bracket-group skip exists because rustc lowers `///` doc
    // comments into `#[doc = "..."]`, and this codebase's doc comments are
    // full of em dashes that never reach a console. But an ARRAY literal
    // has the same delimiter, so keying the skip on "bracket group with no
    // pending identifier" hid real console output inside one.
    assert_eq!(
        scan(r#"fn f() { eprintln!("{:?}", ["bad — output"]); }"#, "x.rs").len(),
        1,
        "an array literal inside a console macro must be scanned"
    );
    assert!(
        scan("#[doc = \"a — dash\"]\nfn f() {}", "x.rs").is_empty(),
        "attribute bodies (including lowered doc comments) stay exempt"
    );
}

#[test]
fn c_string_literal_value_is_checked() {
    // `c"..."` holds UTF-8 and reaches a console via `to_string_lossy()`.
    // It tokenizes atomically, but was falling through the decode match
    // unchecked.
    assert_eq!(
        scan(
            r#"fn f() { eprintln!("{}", c"bad — output".to_string_lossy()); }"#,
            "x.rs"
        )
        .len(),
        1
    );
}

#[test]
fn status_glyphs_are_rejected() {
    // A check mark that renders as `Γ£ô` on a legacy code page is worse
    // than the word it replaced.
    for glyph in ["\u{2713}", "\u{2717}", "\u{26A0}", "\u{25CF}"] {
        let src = format!("fn f() {{ println!(\"{glyph} status\"); }}");
        assert_eq!(
            scan(&src, "x.rs").len(),
            1,
            "glyph {glyph:?} must be caught"
        );
    }
}

#[test]
fn crlf_line_continuation_still_parses() {
    // Windows-only regression: `\` at end of line followed by CRLF is not
    // a valid escape to a lexer that has not normalised line endings, so
    // the literal failed to parse there and parsed fine on Linux. This is
    // real code shape -- long messages wrapped with a continuation are all
    // over this codebase.
    let crlf =
        "fn f() {\r\n    println!(\"wrapped \\\r\n        message \u{2014} here\");\r\n}\r\n";
    assert_eq!(
        scan(crlf, "x.rs").len(),
        1,
        "a CRLF source with a line continuation must parse AND be checked"
    );
}

#[test]
fn dbg_counts_as_console_output_in_ui_modules() {
    // `dbg!` writes to stderr and lands in the same subprocess logs as
    // `eprintln!`, so the ui/ console-macro filter must recognise it.
    assert_eq!(
        scan(r#"fn f() { dbg!("bad \u{2014} output"); }"#, "ui/corpus.rs").len(),
        1
    );
}

#[test]
fn root_ui_module_gets_the_ui_scope() {
    // `src/rust/ui.rs` normalises to "ui.rs" with no `ui/` prefix, so it
    // was being scanned as if every literal were console output -- an
    // eframe window title would have failed the guard.
    assert!(
        scan(
            r#"fn f() { let _t = "whisper-dictate \u{2014} ready"; }"#,
            "ui.rs"
        )
        .is_empty(),
        "rendered strings in the root UI module must be exempt"
    );
    assert_eq!(
        scan(r#"fn f() { eprintln!("bad \u{2014} output"); }"#, "ui.rs").len(),
        1,
        "but its console output must still be checked"
    );
}

#[test]
fn byte_string_escapes_are_decoded() {
    // `b"\xE2\x80\x94"` is an em dash in UTF-8 and reaches a console via
    // `from_utf8_lossy`, even though the source grammar is byte-oriented.
    assert_eq!(
        scan(
            r#"fn f() { eprintln!("{}", String::from_utf8_lossy(b"\xE2\x80\x94")); }"#,
            "x.rs"
        )
        .len(),
        1
    );
}

#[test]
fn console_bearing_attributes_are_scanned_but_doc_comments_are_not() {
    // clap prints `about`/`help` from `--help`, and thiserror's `error`
    // becomes the Display of an error that gets printed. Only `doc` is
    // exempt, and only because rustc lowers `///` into it.
    assert_eq!(
        scan(r#"#[command(about = "a \u{2014} b")] struct C;"#, "x.rs").len(),
        1,
        "clap help text reaches --help"
    );
    assert_eq!(
        scan(r#"#[error("failed \u{2014} retry")] struct E;"#, "x.rs").len(),
        1,
        "thiserror Display reaches the console"
    );
    assert!(
        scan(r#"#[doc = "a \u{2014} dash"] fn f() {}"#, "x.rs").is_empty(),
        "doc comments never reach a console"
    );
}

#[test]
fn nested_cfg_test_item_inside_a_function_is_skipped() {
    // `audio/vad.rs` has `#[cfg(test)]` on individual match ARMS inside a
    // production function. Stripping only at the top level left those
    // synthetic test messages subject to the production guard.
    let src = r#"
        fn f() {
            match x {
                Backend::Real => ok(),
                #[cfg(test)]
                Backend::AlwaysError => Err(anyhow!("synthetic \u{2014} failure")),
            }
        }
    "#;
    assert!(
        scan(src, "x.rs").is_empty(),
        "a cfg(test) match arm is absent from shipping builds"
    );
}

#[test]
fn comments_are_never_reported() {
    let src = "// a — dash in a comment\nfn f() { println!(\"clean\"); }";
    assert!(scan(src, "x.rs").is_empty());
}

#[test]
fn mid_file_cfg_test_item_does_not_hide_production_code_after_it() {
    // `runtime/mod.rs` declares `#[cfg(test)] mod app_root_tests;` at line
    // 105 with shipping entry points below it. Truncating the file at the
    // first test attribute skipped all of them.
    let src = r#"
        #[cfg(test)]
        mod app_root_tests;
        #[cfg(test)]
        fn helper() {}
        pub fn run_terminal() { eprintln!("bad — output"); }
    "#;
    assert_eq!(scan(src, "x.rs").len(), 1);
}

#[test]
fn cfg_test_module_body_is_not_reported() {
    let src = r#"
        #[cfg(test)]
        mod tests { const X: &str = "in — tests"; }
    "#;
    assert!(scan(src, "x.rs").is_empty());
}

#[test]
fn ui_modules_check_console_macros_but_not_rendered_labels() {
    // egui draws from a font atlas, so a label may hold an em dash. But
    // `ui/corpus.rs` really does `eprintln!` on a rejected corpus id, and
    // that reaches a Windows console.
    assert!(
        scan(r#"fn f() { ui.label("nice — label"); }"#, "ui/corpus.rs").is_empty(),
        "rendered labels must stay exempt"
    );
    assert_eq!(
        scan(
            r#"fn f() { eprintln!("corpus: bad — id"); }"#,
            "ui/corpus.rs"
        )
        .len(),
        1,
        "console output inside ui/ must still be checked"
    );
}

#[test]
fn allowlist_entry_still_matches_a_real_literal() {
    // A stale exemption silently widens the guard.
    for (path, needle, reason) in ALLOWLIST {
        assert!(!reason.trim().is_empty(), "{path} needs a reason");
        let full = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path);
        let src = std::fs::read_to_string(&full)
            .unwrap_or_else(|e| panic!("allowlisted {path} unreadable ({e}); drop the entry"));
        assert!(
            src.contains(needle),
            "allowlist needle {needle:?} no longer appears in {path} -- delete the entry"
        );
    }
}
