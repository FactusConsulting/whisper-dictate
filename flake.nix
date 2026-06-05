{
  description = "whisper-dictate — local push-to-talk dictation using Whisper STT";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = inputs: import ./nix/flake.nix inputs;
}
