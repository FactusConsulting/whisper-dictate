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
const CONSOLE_MACROS: &[&str] = &["println", "eprintln", "print", "eprint", "dbg"];

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
        // Byte strings look byte-oriented, but `b"\xE2\x80\x94"` spells an em
        // dash through escapes and reaches a console via `from_utf8_lossy`.
        // Decode when the bytes are valid UTF-8; when they are not, they
        // cannot contain the blocked characters anyway.
        Ok(litrs::Literal::ByteString(b)) => String::from_utf8(b.value().to_vec()).ok(),
        // A single byte is `u8`: it cannot hold a multi-byte character.
        // Numeric and bool literals carry no text.
        Ok(litrs::Literal::Byte(_)) => None,
        Ok(litrs::Literal::Bool(_) | litrs::Literal::Integer(_) | litrs::Literal::Float(_)) => None,
        // `litrs::Literal` is `#[non_exhaustive]`, so a future Rust literal
        // kind lands here. Fail rather than skip: silently returning None is
        // exactly how a new syntax would become an invisible hole -- the
        // failure mode this guard was rewritten to eliminate.
        Ok(_) => panic!("unhandled literal kind {raw:?} -- teach literal_runtime_text about it"),
        Err(err) => panic!("unparseable literal {raw:?}: {err}"),
    }
}

/// True for `#[doc = "..."]`, including the form rustc lowers `///` into.
fn is_doc_attribute(group: &proc_macro2::Group) -> bool {
    matches!(
        group.stream().into_iter().next(),
        Some(TokenTree::Ident(ref i)) if i == "doc"
    )
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
                    // Attribute body: `#[..]` / `#![..]`.
                    //
                    // Only `doc` is exempt, and only because rustc lowers
                    // `///` into `#[doc = "..."]` -- this codebase's doc
                    // comments are full of em dashes that never reach a
                    // console. Every OTHER attribute is scanned, because
                    // several carry real console text: clap's
                    // `#[command(about = "...")]` and `#[arg(help = "...")]`
                    // are printed by `--help`, and thiserror's
                    // `#[error("...")]` becomes the Display of an error that
                    // gets printed. An ARRAY literal reaches this arm with
                    // `after_pound == false` and is scanned either way.
                    after_pound = false;
                    if is_doc_attribute(&group) {
                        continue;
                    }
                    scan_tokens(group.stream(), file, console_only, in_console, out);
                    pending_macro = None;
                    continue;
                }
                after_pound = false;
                scan_tokens(
                    // Strip at EVERY level, not just the top: `audio/vad.rs`
                    // has `#[cfg(test)]` on individual match ARMS inside a
                    // production function, and those are absent from shipping
                    // builds.
                    strip_cfg_test(group.stream()),
                    file,
                    console_only,
                    nested_in_console,
                    out,
                );
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

/// True for the egui modules, where only console-macro arguments are checked.
///
/// Covers `ui/**` AND the root `ui.rs`, which normalises without the prefix --
/// it was previously having its rendered strings (window titles) checked as if
/// they were console output.
///
/// Shared with the behaviour tests on purpose: a test computing this itself
/// would be asserting against a copy of the rule rather than the rule.
fn is_ui_scope(rel: &str) -> bool {
    rel.starts_with("ui/") || rel == "ui.rs"
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
    let console_only = is_ui_scope(&rel);
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
#[path = "console_ascii_behaviour_tests.rs"]
mod guard_behaviour;
