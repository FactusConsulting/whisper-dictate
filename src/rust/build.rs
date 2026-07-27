fn main() {
    // Declare `whisper-rs-cuda` as an expected Cargo feature name so
    // `cfg!(feature = "whisper-rs-cuda")` in `whisper::device_options`
    // does not trip the `unexpected_cfgs` lint before the concurrent
    // CUDA-build-flag change adds the feature to Cargo.toml. When that
    // change lands the feature is declared for real; this hint stays a
    // harmless no-op (rustc unions the two allow-lists).
    println!("cargo:rustc-check-cfg=cfg(feature, values(\"whisper-rs-cuda\"))");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("../../assets/whisper-dictate.ico");
    if let Err(err) = resource.compile() {
        panic!("failed to embed Windows application icon: {err}");
    }
}
