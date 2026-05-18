# NixOS module for whisper-dictate — handles ydotool, input group, and autostart.
# Usage in configuration.nix:
#
#   imports = [ (builtins.getFlake "github:FactusConsulting/whisper-dictate").nixosModules.default ];
#   services.whisperDictate.enable = true;
#   services.whisperDictate.users  = [ "yourname" ];
#
{ config, lib, pkgs, ... }:

let
  cfg = config.services.whisperDictate;
in {
  options.services.whisperDictate = {
    enable = lib.mkEnableOption "whisper-dictate push-to-talk dictation";

    users = lib.mkOption {
      type        = lib.types.listOf lib.types.str;
      default     = [];
      description = "Users to add to the 'input' group for evdev hotkey detection.";
    };

    package = lib.mkOption {
      type        = lib.types.package;
      description = "The whisper-dictate package to use.";
    };
  };

  config = lib.mkIf cfg.enable {
    # ydotoold — virtual uinput keyboard daemon for Wayland text injection.
    services.ydotool.enable = true;

    # /dev/uinput access for the 'input' group is provided by
    # services.ydotool.enable above (it ships the uinput udev rule on
    # current nixpkgs), so no hand-rolled services.udev.extraRules here.

    # Add specified users to the input group.
    users.users = lib.listToAttrs (map (u: {
      name  = u;
      value = { extraGroups = [ "input" ]; };
    }) cfg.users);

    # Make the package available system-wide.
    environment.systemPackages = [ cfg.package ];
  };
}
