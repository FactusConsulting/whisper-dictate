{ self, nixpkgs }:
let
  # The native package currently depends on Linux desktop/audio libraries and
  # nixpkgs only exposes the required system ONNX Runtime on Linux. Do not
  # advertise Darwin derivations that cannot link the audio-in-rust route.
  systems = [ "x86_64-linux" "aarch64-linux" ];
  forAllSystems = nixpkgs.lib.genAttrs systems;
in {
  # --- packages --------------------------------------------------------------
  packages = forAllSystems (system:
    let
      pkgs = nixpkgs.legacyPackages.${system};
      package = pkgs.callPackage ./package.nix { src = self; };
    in {
      default = package;
      whisper-dictate = package;
    });

  # --- apps (nix run) --------------------------------------------------------
  apps = forAllSystems (system: {
    default = {
      type = "app";
      program = "${self.packages.${system}.default}/bin/whisper-dictate";
    };
  });

  # --- NixOS module ----------------------------------------------------------
  nixosModules.default = { config, lib, pkgs, ... }:
    (import ./module.nix { inherit config lib pkgs; }) // {
      # Inject the flake package as the default for the module option.
      config = lib.mkIf config.services.whisperDictate.enable {
        services.whisperDictate.package =
          lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      };
    };
}
