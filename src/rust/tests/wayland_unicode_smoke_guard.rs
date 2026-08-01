//! Guards the Rust-only Unicode clipboard readiness section in the canonical
//! Wayland user smoke.

mod common;

use common::read_wayland_smoke;

#[test]
fn unicode_readiness_is_native_only_before_probing_helpers() {
    let smoke = read_wayland_smoke();
    let section = smoke
        .split("section \"native Rust Wayland Unicode auto-paste readiness\"")
        .nth(1)
        .expect("Unicode readiness section must exist")
        .split("section \"history last / reinject-last (dry-run)\"")
        .next()
        .expect("Unicode readiness section must terminate");

    assert!(
        !section.contains("CMD_MODE\" = \"python\"") && !section.contains("Python fallback"),
        "Unicode readiness must not retain the retired compatibility branch"
    );
    let helper = section
        .find("command -v wl-copy")
        .expect("readiness must probe the native Wayland clipboard helper");
    assert!(
        helper > 0,
        "native clipboard helper probe must be in the section"
    );
}
