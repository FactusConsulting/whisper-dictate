use super::*;

#[test]
fn ubuntu_setup_script_path_prefers_linux_packaging_directory() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir
        .path()
        .join("packaging")
        .join("linux")
        .join("ubuntu26.04")
        .join("setup.sh");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, "").unwrap();

    assert_eq!(ubuntu_setup_script_path(dir.path()), script);
}

#[test]
fn ubuntu_setup_script_path_falls_back_to_legacy_bundle_directory() {
    let dir = tempfile::tempdir().unwrap();

    assert_eq!(
        ubuntu_setup_script_path(dir.path()),
        dir.path().join("ubuntu26.04").join("setup.sh")
    );
}
