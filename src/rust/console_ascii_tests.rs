//! Guard: no typographic non-ASCII in strings that reach a console.
//!
//! AGENTS.md: "Console output is ASCII- or UTF-8-safe. New stdout/stderr
//! lines, log messages, and installer scripts must work under PowerShell,
//! cmd.exe, hidden launchers, and the Rust UI's subprocess logs. Non-ASCII
//! without a tested fallback is a defect."
//!
//! The rule was declared but unenforced, so it leaked: 30 production strings
//! carried em dashes and arrows before this landed, and a fresh one slipped
//! into #574 before review caught it. cmd.exe on a legacy code page renders
//! those as mojibake.
//!
//! # Why this is a Rust test and not a script
//!
//! The first cut of this guard hand-rolled a Rust lexer in Python. Review
//! found six separate ways to slip a literal past it -- raw strings whose
//! hash count is not matched (`r##"has "# inside"##`), `br"..."` and
//! `cr#"..."#` prefixes, `'"'` char literals desynchronising the scanner for
//! the rest of the file, nested block comments, and `\u{2014}` escapes that
//! spell an em dash without containing one. Each fix invited the next.
//!
//! Writing a Rust lexer is a solved problem, so this uses the real one.
//! `proc_macro2` tokenizes; `litrs` decodes the literal to its RUNTIME value.
//! Every one of those six holes closes by construction rather than by
//! enumeration, and the guard can no longer pass vacuously because a
//! construct nobody thought of desynced a parser.
//!
//! # Why a blocklist, not a blanket ASCII check
//!
//! Most non-ASCII here is legitimate and must keep working: Danish
//! hallucination phrases, per-locale keyboard layout data (Nordic, Cyrillic,
//! Polish), dictionary stopwords, `Strasse` casefold fixtures. A blanket
//! `is_ascii()` sweep would flag all of it and be deleted within a week.
//!
//! So this blocks a small set of TYPOGRAPHIC PUNCTUATION. It contains no
//! letters, which is what makes it safe: every language-data string above
//! passes untouched, while the characters people reach for when writing prose
//! in an error message are caught. Each has a free ASCII equivalent.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, TokenStream, TokenTree};

/// Punctuation only -- deliberately no letters. See the module docs.
const TYPOGRAPHIC: &[(char, &str)] = &[
    ('\u{2014}', "-"),   // em dash
    ('\u{2013}', "-"),   // en dash
    ('\u{2192}', "->"),  // rightwards arrow
    ('\u{2190}', "<-"),  // leftwards arrow
    ('\u{201C}', "\""),  // left double quote
    ('\u{201D}', "\""),  // right double quote
    ('\u{2018}', "'"),   // left single quote
    ('\u{2019}', "'"),   // right single quote
    ('\u{2026}', "..."), // ellipsis
    ('\u{2264}', "<="),  //
    ('\u{2265}', ">="),  //
    ('\u{2260}', "!="),  //
    ('\u{00D7}', "x"),   // multiplication sign
    ('\u{2022}', "-"),   // bullet
    // Status glyphs. These are what people actually reach for when dressing
    // up CLI output, and they garble in exactly the legacy-code-page consoles
    // this guard exists for -- a check mark that renders as `Γ£ô` is worse
    // than the word it replaced.
    ('\u{2713}', "OK"),   // check mark
    ('\u{2714}', "OK"),   // heavy check mark
    ('\u{2717}', "FAIL"), // ballot X
    ('\u{2718}', "FAIL"), // heavy ballot X
    ('\u{26A0}', "WARN"), // warning sign
    ('\u{2139}', "INFO"), // information source
    ('\u{25CF}', "*"),    // black circle
    ('\u{25CB}', "o"),    // white circle
];

fn ascii_equivalent(c: char) -> Option<&'static str> {
    TYPOGRAPHIC
        .iter()
        .find(|(bad, _)| *bad == c)
        .map(|(_, good)| *good)
}

/// Macros whose arguments land on a real console.
///
/// In `ui/**` ONLY these are checked: egui draws its labels from a font atlas
/// and never touches a code page, so an em dash in a rendered string is fine.
/// But `ui/corpus.rs` really does `eprintln!` when rejecting an unsafe corpus
/// id, and that DOES reach a Windows console -- a blanket module exemption
/// would have covered it (caught in review of #576).
const CONSOLE_MACROS: &[&str] = &["println", "eprintln", "print", "eprint"];

/// Exemptions scoped to a specific literal, never a whole file: a file-wide
/// entry would silently cover every string added to it later.
const ALLOWLIST: &[(&str, &str, &str)] = &[(
    "postprocess/prompt.rs",
    "bliver til",
    "regex that MATCHES a user typing an arrow in a dictionary prompt -- \
     input parsing, not console output; dropping it would silently stop \
     recognising prompts people already write",
)];

struct Violation {
    file: String,
    line: usize,
    bad: char,
    suggestion: &'static str,
    snippet: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: {:?} (use {:?}) in {}",
            self.file, self.line, self.bad, self.suggestion, self.snippet
        )
    }
}

/// Decode a literal token to the string it produces at RUNTIME.
///
/// Going through `litrs` rather than the source text is what closes the
/// `\u{2014}` hole: source containing no em dash still emits one.
fn literal_runtime_text(token: &proc_macro2::Literal) -> Option<String> {
    let raw = token.to_string();
    match litrs::Literal::parse(raw.clone()) {
        Ok(litrs::Literal::String(s)) => Some(s.value().to_owned()),
        Ok(litrs::Literal::Char(c)) => Some(c.value().to_string()),
        // C strings hold UTF-8 and reach a console via `to_string_lossy()`,
        // so they are checked like any other text literal. Lossy on purpose:
        // a literal that is not valid UTF-8 cannot contain the blocked
        // characters anyway, and this must never panic on odd input.
        Ok(litrs::Literal::CString(c)) => {
            Some(String::from_utf8_lossy(c.value().to_bytes()).into_owned())
        }
        // Byte and byte-string literals are `u8`-only: the Rust grammar
        // forbids non-ASCII in them outright, so there is nothing to check.
        // Numeric and bool literals carry no text.
        Ok(litrs::Literal::Byte(_) | litrs::Literal::ByteString(_)) => None,
        Ok(litrs::Literal::Bool(_) | litrs::Literal::Integer(_) | litrs::Literal::Float(_)) => None,
        // `litrs::Literal` is `#[non_exhaustive]`, so a future Rust literal
        // kind lands here. Fail rather than skip: silently returning None is
        // exactly how a new syntax would become an invisible hole -- the
        // failure mode this guard was rewritten to eliminate.
        Ok(_) => panic!("unhandled literal kind {raw:?} -- teach literal_runtime_text about it"),
        Err(err) => panic!("unparseable literal {raw:?}: {err}"),
    }
}

/// Walk a token stream, collecting violations.
///
/// `console_only` restricts checking to [`CONSOLE_MACROS`] arguments, used for
/// `ui/**`. `in_console` tracks whether the current group IS such an argument.
fn scan_tokens(
    stream: TokenStream,
    file: &str,
    console_only: bool,
    in_console: bool,
    out: &mut Vec<Violation>,
) {
    let mut pending_macro: Option<String> = None;
    // True immediately after a `#` (or `#` `!`), i.e. the next bracket group
    // is an ATTRIBUTE body rather than an array literal. Without this, every
    // bracket group with no pending identifier looked like an attribute, so
    // `eprintln!("{:?}", ["bad — output"])` was skipped -- real console output
    // hidden by the array's own brackets (caught in review of #576).
    let mut after_pound = false;
    for tree in stream {
        match tree {
            TokenTree::Ident(ident) => {
                after_pound = false;
                let name = ident.to_string();
                // `#[cfg(test)] mod x { .. }` is skipped wholesale -- but by
                // its GROUP, not by truncating the file. A bare `#[cfg(test)]`
                // on one mid-file item, or a test module declared before
                // production code (`runtime/mod.rs` has both), must not hide
                // everything after it. Review of #576 found exactly that.
                if name == "mod" {
                    // `mod x;` or `mod x { .. }` -- consume the name, then the
                    // body if it has one. Whether it is cfg(test) is decided
                    // by the caller having seen the attribute.
                    pending_macro = None;
                    continue;
                }
                pending_macro = Some(name);
            }
            TokenTree::Punct(punct) => {
                match punct.as_char() {
                    // `#` opens an attribute; `#!` is the inner form, so `!`
                    // must not clear the flag.
                    '#' => after_pound = true,
                    '!' => {}
                    _ => after_pound = false,
                }
                if punct.as_char() != '!' {
                    pending_macro = None;
                }
            }
            TokenTree::Group(group) => {
                let is_console_call = pending_macro
                    .as_deref()
                    .is_some_and(|m| CONSOLE_MACROS.contains(&m));
                let nested_in_console = in_console || is_console_call;
                if group.delimiter() == Delimiter::Bracket && after_pound {
                    // Attribute body: `#[..]` / `#![..]`. Skipped because
                    // rustc lowers `///` doc comments into `#[doc = "..."]`,
                    // and this codebase's doc comments are full of em dashes
                    // that never reach a console. An ARRAY literal reaches
                    // this arm with `after_pound == false` and is scanned.
                    after_pound = false;
                    continue;
                }
                after_pound = false;
                scan_tokens(group.stream(), file, console_only, nested_in_console, out);
                pending_macro = None;
            }
            TokenTree::Literal(lit) => {
                after_pound = false;
                if console_only && !in_console {
                    pending_macro = None;
                    continue;
                }
                let Some(text) = literal_runtime_text(&lit) else {
                    pending_macro = None;
                    continue;
                };
                let exempt = ALLOWLIST.iter().any(|(path, needle, _)| {
                    file.replace('\\', "/").ends_with(path) && text.contains(needle)
                });
                if !exempt {
                    let bad: BTreeSet<char> = text
                        .chars()
                        .filter(|c| ascii_equivalent(*c).is_some())
                        .collect();
                    for c in bad {
                        out.push(Violation {
                            file: file.to_owned(),
                            line: lit.span().start().line,
                            bad: c,
                            suggestion: ascii_equivalent(c).unwrap_or("-"),
                            snippet: text
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ")
                                .chars()
                                .take(70)
                                .collect(),
                        });
                    }
                }
                pending_macro = None;
            }
        }
    }
}

/// Strip `#[cfg(test)] mod name { .. }` blocks and `#[cfg(test)]`-gated items
/// before tokenizing.
///
/// Done on the token stream rather than by slicing text, so a test module in
/// the MIDDLE of a file (`runtime/mod.rs:105`) removes only itself and leaves
/// the production code after it fully scanned.
fn strip_cfg_test(stream: TokenStream) -> TokenStream {
    let mut out = Vec::new();
    let mut tokens = stream.into_iter().peekable();
    while let Some(tree) = tokens.next() {
        if let TokenTree::Punct(ref p) = tree {
            if p.as_char() == '#' {
                // `#![cfg(test)]` -- an INNER attribute, so the whole
                // enclosing file/module is test-only. Tokenizes as `#` `!`
                // `[..]`, which the outer-attribute branch below does not
                // match. `injection/self_test/scenarios.rs` has this shape.
                let inner =
                    matches!(tokens.peek(), Some(TokenTree::Punct(b)) if b.as_char() == '!');
                let mut lookahead = tokens.clone();
                if inner {
                    lookahead.next();
                }
                if let Some(TokenTree::Group(g)) = lookahead.peek() {
                    if g.delimiter() == Delimiter::Bracket
                        && g.stream()
                            .to_string()
                            .replace(' ', "")
                            .starts_with("cfg(test)")
                    {
                        if inner {
                            return TokenStream::new();
                        }
                        tokens.next(); // the `[cfg(test)]` group
                        skip_one_item(&mut tokens);
                        continue;
                    }
                }
            }
        }
        out.push(tree);
    }
    out.into_iter().collect()
}

/// Consume exactly one item following a `#[cfg(test)]` attribute: tokens up to
/// and including either a brace group (`mod x { .. }`, `fn f() { .. }`) or a
/// terminating `;` (`mod x;`).
fn skip_one_item(tokens: &mut std::iter::Peekable<proc_macro2::token_stream::IntoIter>) {
    for tree in tokens.by_ref() {
        match tree {
            TokenTree::Group(g) if g.delimiter() == Delimiter::Brace => return,
            TokenTree::Punct(p) if p.as_char() == ';' => return,
            _ => {}
        }
    }
}

fn is_test_source(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let full = path.to_string_lossy().replace('\\', "/");
    name.ends_with("_tests.rs")
        || name == "tests.rs"
        || name.starts_with("tests_")
        || full.contains("/tests/")
        || full.contains("test_support")
}

fn rust_sources(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) == Some("target") {
                continue;
            }
            rust_sources(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Normalise CRLF before lexing, exactly as rustc does when it reads a source
/// file.
///
/// Git checks these out with CRLF on Windows, and a string continuation (a
/// trailing `\` before the newline) then reads as `\` + CR, which is not a
/// valid escape -- so the literal fails to parse on Windows and parses fine
/// everywhere else. CI found this the moment an unparseable literal started
/// FAILING instead of being skipped: before that, every literal using a line
/// continuation was silently unchecked on Windows only.
fn normalize_line_endings(src: &str) -> String {
    src.replace("\r\n", "\n")
}

fn scan_file(path: &Path) -> Vec<Violation> {
    // Normalise CRLF before lexing, exactly as rustc does when it reads a
    // source file. Git checks these out with CRLF on Windows, and a string
    // continuation (`\` at end of line) then reads as `\` + CR, which is not
    // a valid escape -- so the literal fails to parse on Windows and parses
    // fine everywhere else. Found by CI the moment an unparseable literal
    // started failing instead of being skipped: before that, every literal
    // using a line continuation was silently unchecked on Windows only.
    let src = normalize_line_endings(&std::fs::read_to_string(path).expect("readable source"));
    let Ok(stream) = src.parse::<TokenStream>() else {
        // A file the lexer rejects would silently contribute nothing, so fail
        // loudly rather than pass vacuously.
        panic!("{} did not tokenize", path.display());
    };
    let rel = path.to_string_lossy().replace('\\', "/");
    let rel = rel
        .rsplit_once("/src/rust/")
        .map_or(rel.clone(), |(_, r)| r.to_owned());
    let console_only = rel.starts_with("ui/");
    let mut out = Vec::new();
    scan_tokens(strip_cfg_test(stream), &rel, console_only, false, &mut out);
    out
}

#[test]
fn no_typographic_punctuation_in_console_strings() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_sources(&root, &mut files);
    files.sort();
    assert!(files.len() > 100, "source walk found only {}", files.len());

    let mut violations = Vec::new();
    for path in &files {
        if is_test_source(path) {
            continue;
        }
        violations.extend(scan_file(path));
    }
    let report: Vec<String> = violations.iter().map(|v| v.to_string()).collect();
    assert!(
        report.is_empty(),
        "Typographic punctuation in strings that can reach stdout/stderr; \
         these garble under cmd.exe on a legacy code page (AGENTS.md). \
         Replace with the ASCII equivalent shown:\n{}",
        report.join("\n")
    );
}

#[cfg(test)]
mod guard_behaviour {
    use super::*;

    fn scan(src: &str, rel: &str) -> Vec<String> {
        let stream = normalize_line_endings(src)
            .parse::<TokenStream>()
            .expect("tokenizes");
        let mut out = Vec::new();
        scan_tokens(
            strip_cfg_test(stream),
            rel,
            rel.starts_with("ui/"),
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
}
