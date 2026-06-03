fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("../../assets/whisper-dictate.ico");
    if let Err(err) = resource.compile() {
        panic!("failed to embed Windows application icon: {err}");
    }
}
