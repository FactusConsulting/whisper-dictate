# whisper-dictate Nix derivation.
# Used by flake.nix (src = self) and can be submitted to nixpkgs (src = fetchFromGitHub).
{ lib
, python3
, makeWrapper
, stdenv
, fetchFromGitHub
, src ? null          # overridden by flake to use repo root
, version ? "0.2.23"
}:

let
  # Resolve source: flake passes src = self, nixpkgs sets it via fetchFromGitHub.
  resolvedSrc = if src != null then src else fetchFromGitHub {
    owner = "FactusConsulting";
    repo  = "whisper-dictate";
    rev   = "v${version}";
    hash  = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="; # filled by nixpkgs maintainer
  };

  pythonEnv = python3.withPackages (ps: with ps; [
    faster-whisper
    requests
    numpy
    sounddevice
    pynput
    pyperclip
  ] ++ lib.optionals stdenv.isLinux [
    evdev
    scipy
  ]);

in stdenv.mkDerivation {
  pname   = "whisper-dictate";
  inherit version;
  src     = resolvedSrc;

  nativeBuildInputs = [ makeWrapper ];

  dontBuild = true;

  installPhase = ''
    runHook preInstall

    install -Dm644 voice_pi.py $out/lib/whisper-dictate/voice_pi.py

    makeWrapper ${pythonEnv}/bin/python3 $out/bin/whisper-dictate \
      --add-flags "$out/lib/whisper-dictate/voice_pi.py" \
      --set VOICEPI_SKIP_SYSCHECK 1

    runHook postInstall
  '';

  meta = with lib; {
    description  = "Local push-to-talk dictation — speak instead of typing";
    longDescription = ''
      App-agnostic push-to-talk dictation. Hold a key, speak, release — the
      transcribed text is injected into whatever window has focus. Whisper runs
      fully locally; nothing leaves the machine.

      On NixOS/Wayland: install ydotool and add your user to the input group,
      or use the provided NixOS module in nix/module.nix.
    '';
    homepage     = "https://github.com/FactusConsulting/whisper-dictate";
    license      = licenses.mit;
    maintainers  = [];
    platforms    = platforms.unix;
    mainProgram  = "whisper-dictate";
  };
}
