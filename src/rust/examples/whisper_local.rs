//! Manual smoke test for the local Whisper integration.
//!
//! Build & run (the feature flag pulls in whisper.cpp via `whisper-rs`, so
//! you need CMake + a C/C++ compiler on the build host):
//!
//! ```sh
//! cargo run --release --features whisper-rs-local --example whisper_local -- \
//!     --model /path/to/ggml-tiny.en.bin \
//!     --wav   /path/to/audio_16khz_mono.wav \
//!     [--language da]                    # optional; omit or "auto" → auto-detect
//!     [--initial-prompt "..."]           # optional context hint
//! ```
//!
//! This is intentionally tiny: it proves the library API works end-to-end
//! from the command line. Runtime wiring (replacing/augmenting the Python
//! transcription path) is a later sub-task of roadmap issue #317.

#[cfg(not(feature = "whisper-rs-local"))]
fn main() {
    eprintln!(
        "this example requires `--features whisper-rs-local` \
         (see src/rust/whisper/mod.rs)"
    );
    std::process::exit(2);
}

#[cfg(feature = "whisper-rs-local")]
fn main() -> anyhow::Result<()> {
    use std::path::PathBuf;

    use clap::Parser;
    use whisper_dictate_app::whisper::LocalWhisper;

    #[derive(Parser, Debug)]
    #[command(about = "Transcribe a 16 kHz mono WAV with a local Whisper model")]
    struct Args {
        /// Path to a whisper.cpp GGML model file (`ggml-*.bin`).
        /// GGUF models are not supported by whisper-rs.
        #[arg(long)]
        model: PathBuf,
        /// Path to a 16 kHz mono PCM WAV file.
        #[arg(long)]
        wav: PathBuf,
        /// Optional language code (e.g. `en`, `da`, `de`). Omit or pass
        /// `auto` to auto-detect with a multilingual model. The `.en`
        /// models are English-only regardless of this flag.
        #[arg(long)]
        language: Option<String>,
        /// Optional context hint fed to whisper.cpp before decoding
        /// (jargon, names, custom dictionary terms).
        #[arg(long)]
        initial_prompt: Option<String>,
    }

    let args = Args::parse();
    let whisper = LocalWhisper::new(&args.model)?;
    let text = whisper.transcribe_wav(
        &args.wav,
        args.language.as_deref(),
        args.initial_prompt.as_deref(),
    )?;
    println!("{}", text.trim());
    Ok(())
}
