{ self, nixpkgs }:
let
  systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
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

  # --- dev shell -------------------------------------------------------------
  devShells = forAllSystems (system:
    let pkgs = nixpkgs.legacyPackages.${system};
    in {
      default = pkgs.mkShell {
        packages = with pkgs; [
          (python3.withPackages (ps: with ps;
            [
              faster-whisper
              requests
              numpy
              sounddevice
              pynput
              pyperclip
            ] ++ nixpkgs.lib.optionals stdenv.isLinux [
              evdev
            ]))
        ];
      };
    });
}
