# whisper-dictate Nix derivation.
# Used by nix/flake.nix (src = self) and can be submitted to nixpkgs (src = fetchFromGitHub).
#
# TODO Wave-8 follow-up: rewrite this derivation to build the Rust binary
# from source (cargo build --release --features rust-injection,rust-hotkeys,
# audio-in-rust,whisper-rs-vulkan). The Python runtime shipped by this file
# was deleted in Wave 8 of #348, but a proper rustPlatform.buildRustPackage
# recipe (with CMake for whisper.cpp + Vulkan optional) is a substantial
# rewrite that lives in its own PR. Until then, Nix/NixOS users should
# build with `cargo build` directly and install the resulting binary from
# `target/release/whisper-dictate`.
{ lib, stdenv, ... }:

stdenv.mkDerivation {
  pname   = "whisper-dictate";
  version = "1.20.5";
  # Placeholder: real recipe pending Wave 8 follow-up (see comment above).
  dontUnpack = true;
  dontBuild  = true;
  installPhase = ''
    mkdir -p $out
    cat > $out/README <<EOF
    whisper-dictate: pending Nix packaging rewrite (Wave 8 of #348).
    Build from source: cargo build --release --manifest-path src/rust/Cargo.toml       --features rust-injection,rust-hotkeys,audio-in-rust,whisper-rs-vulkan
    EOF
  '';

  meta = with lib; {
    description = "Local push-to-talk dictation (Rust) - Nix recipe pending rewrite";
    homepage    = "https://github.com/FactusConsulting/whisper-dictate";
    license     = licenses.mit;
    platforms   = platforms.unix;
    mainProgram = "whisper-dictate";
  };
}
