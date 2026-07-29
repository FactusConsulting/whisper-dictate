//! Guards the Rust-only Unicode clipboard readiness section in the canonical
//! Wayland user smoke.

mod common;

use common::read_wayland_smoke;

#[test]
fn unicode_readiness_skips_the_python_fallback_before_probing_helpers() {
    let smoke = read_wayland_smoke();
    let section = smoke
        .split("section \"native Rust Wayland Unicode auto-paste readiness\"")
        .nth(1)
        .expect("Unicode readiness section must exist")
        .split("section \"history last / reinject-last (dry-run)\"")
        .next()
        .expect("Unicode readiness section must terminate");

    let python = section
        .find("[ \"$CMD_MODE\" = \"python\" ]")
        .expect("Rust-only readiness must test CMD_MODE=python");
    let helper = section
        .find("command -v wl-copy")
        .expect("readiness must probe the native Wayland clipboard helper");
    assert!(
        python < helper,
        "Python fallback must warn-skip before any native clipboard requirement"
    );
}
