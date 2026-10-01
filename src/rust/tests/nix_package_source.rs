mod common;

#[test]
fn nix_package_requires_explicit_source_and_flake_supplies_it() {
    let root = common::repo_root();
    let package = std::fs::read_to_string(root.join("nix/package.nix")).unwrap();
    let flake = std::fs::read_to_string(root.join("nix/flake.nix")).unwrap();
    assert!(
        package.lines().any(|line| line.trim() == ", src"),
        "source must be a required argument"
    );
    assert!(!package.contains("src ? null"));
    assert!(!package.contains("sha256-AAAAAAAA"));
    assert!(!package.contains("fetchFromGitHub"));
    assert!(package.contains("inherit version src;"));
    assert!(package.contains("cargoLock.lockFile = \"${src}/src/rust/Cargo.lock\""));
    assert!(flake.contains("{ src = self; }"));
    assert!(package.contains("\"$out/bin/wd\""));
    assert!(package.contains("\"$out/bin/wd-gui\""));
}
