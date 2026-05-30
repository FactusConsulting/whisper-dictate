# whisper-dictate Nix derivation.
# Used by flake.nix (src = self) and can be submitted to nixpkgs (src = fetchFromGitHub).
{ lib
, python3
, makeWrapper
, stdenv
, fetchFromGitHub
, ydotool          # Wayland text injection (ydotool + ydotoold)
, xdotool          # XWayland/X11 window refocus
, xclip            # X11 clipboard for --paste (pyperclip backend)
, wl-clipboard     # Wayland clipboard (future --paste path)
, alsa-utils       # arecord PipeWire capture path
, src ? null          # overridden by flake to use repo root
, version ? "0.2.45"
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

  # External CLI tools voice_pi.py shells out to. The X11 vs Wayland
  # choice is made at RUNTIME (per session, via $WAYLAND_DISPLAY) — never
  # at build/download time — so the package ships BOTH paths' tools and
  # the app picks one each time it starts. Linux-only.
  runtimeTools = lib.optionals stdenv.isLinux [
    ydotool xdotool xclip wl-clipboard alsa-utils
  ];

in stdenv.mkDerivation {
  pname   = "whisper-dictate";
  inherit version;
  src     = resolvedSrc;

  nativeBuildInputs = [ makeWrapper ];

  dontBuild = true;

  installPhase = ''
    runHook preInstall

    install -Dm644 voice_pi.py $out/lib/whisper-dictate/voice_pi.py
    for _m in vp_*.py; do
      [ -e "$_m" ] && install -Dm644 "$_m" "$out/lib/whisper-dictate/$_m"
    done

    makeWrapper ${pythonEnv}/bin/python3 $out/bin/whisper-dictate \
      --add-flags "$out/lib/whisper-dictate/voice_pi.py" \
      --set VOICEPI_SKIP_SYSCHECK 1 \
      --prefix PATH : ${lib.makeBinPath runtimeTools}

    runHook postInstall
  '';

  meta = with lib; {
    description  = "Local push-to-talk dictation — speak instead of typing";
    longDescription = ''
      App-agnostic push-to-talk dictation. Hold a key, speak, release — the
      transcribed text is injected into whatever window has focus. Whisper runs
      fully locally; nothing leaves the machine.

      X11 and Wayland are both supported; the active backend is chosen at
      runtime (per session) from $WAYLAND_DISPLAY — no build-time switch.

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
