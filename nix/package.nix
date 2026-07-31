# whisper-dictate native Rust derivation.
# Used by nix/flake.nix (src = self) and suitable for nixpkgs with src fetched.
{ lib
, rustPlatform
, makeWrapper
, pkg-config
, cmake
, clang
, libclang
, stdenv
, fetchFromGitHub
, dbus
, wayland
, libx11
, libxcb
, libxkbcommon
, libXi
, libXtst
, alsa-lib
, onnxruntime
, ydotool
, xdotool
, xclip
, wl-clipboard
, src ? null
, version ? "1.22.4"
}:

let
  resolvedSrc = if src != null then src else fetchFromGitHub {
    owner = "FactusConsulting";
    repo = "whisper-dictate";
    rev = "v${version}";
    hash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
  };

  runtimeTools = lib.optionals stdenv.isLinux [
    ydotool
    xdotool
    xclip
    wl-clipboard
  ];
in
rustPlatform.buildRustPackage {
  pname = "whisper-dictate";
  inherit version;
  src = resolvedSrc;

  cargoRoot = "src/rust";
  cargoLock.lockFile = "${resolvedSrc}/src/rust/Cargo.lock";

  nativeBuildInputs = [
    makeWrapper
    pkg-config
    cmake
    clang
  ];

  buildInputs = lib.optionals stdenv.isLinux [
    alsa-lib
    dbus
    wayland
    libx11
    libxcb
    libxkbcommon
    libXi
    libXtst
    onnxruntime
  ];

  cargoBuildFlags = [
    "--features"
    "whisper-rs-local,audio-in-rust,rust-hotkeys,rust-injection"
  ];

  # bindgen (whisper.cpp) and ort-sys both need explicit immutable Nix paths.
  LIBCLANG_PATH = lib.makeLibraryPath [ libclang ];
  ORT_STRATEGY = "system";
  ORT_LIB_LOCATION = lib.optionalString stdenv.isLinux "${onnxruntime}/lib";

  postInstall = lib.optionalString stdenv.isLinux ''
    wrapProgram "$out/bin/whisper-dictate" \
      --prefix PATH : ${lib.makeBinPath runtimeTools}
    wrapProgram "$out/bin/whisper-dictate-gui" \
      --prefix PATH : ${lib.makeBinPath runtimeTools}
  '';

  meta = with lib; {
    description = "Local push-to-talk dictation using native Whisper";
    longDescription = ''
      App-agnostic push-to-talk dictation. Hold a key, speak, release, and the
      transcribed text is injected into the focused window. The packaged
      runtime is the native Rust controller with whisper.cpp inference.
    '';
    homepage = "https://github.com/FactusConsulting/whisper-dictate";
    license = licenses.mit;
    maintainers = [];
    platforms = platforms.unix;
    mainProgram = "whisper-dictate";
  };
}
