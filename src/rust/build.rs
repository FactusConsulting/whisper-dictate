/// The cargo features the native dictation session needs together: local
/// whisper, text injection, the global hotkey and microphone capture. The
/// session is reachable only through the hotkey sink and records only through
/// audio capture, so code for it is compiled only when all four are on;
/// `cfg(native_session)` names that set so no partial combination compiles
/// session code nothing can reach.
const NATIVE_SESSION_FEATURES: [&str; 4] = [
    "WHISPER_RS_LOCAL",
    "RUST_INJECTION",
    "RUST_HOTKEYS",
    "AUDIO_CAPTURE",
];

fn main() {
    println!("cargo::rustc-check-cfg=cfg(native_session)");
    if NATIVE_SESSION_FEATURES
        .iter()
        .all(|feature| std::env::var_os(format!("CARGO_FEATURE_{feature}")).is_some())
    {
        println!("cargo::rustc-cfg=native_session");
    }

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("../../assets/whisper-dictate.ico");
    if let Err(err) = resource.compile() {
        panic!("failed to embed Windows application icon: {err}");
    }
}
