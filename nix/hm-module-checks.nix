{ self, inputs, ... }:
{
  # Eval-only assertions for homeModules.default; `nix flake check` runs them.
  perSystem = { pkgs, lib, ... }:
    let
      hmConfiguration = modules: inputs.home-manager.lib.homeManagerConfiguration {
        inherit pkgs;
        modules = [
          self.homeModules.default
          {
            home = {
              username = "check";
              homeDirectory = "/home/check";
              stateVersion = "24.11";
            };
            programs.niri-dynamic-workspaces.enable = true;
          }
        ] ++ modules;
      };

      standalone = hmConfiguration [ ];
      withNiriFlake = modules:
        (hmConfiguration ([ inputs.niri-flake.homeModules.config ] ++ modules)).config;
      bindKeys = config: lib.attrNames config.programs.niri.settings.binds;
      noKeybinds.programs.niri-dynamic-workspaces = {
        keybind = null;
        deleteKeybind = null;
        moveWindowKeybind = null;
      };

      expect = name: actual: expected:
        lib.assertMsg (actual == expected)
          "${name}: expected ${builtins.toJSON expected}, got ${builtins.toJSON actual}";
    in
    {
      checks.hm-module =
        assert expect "standalone evaluates"
          (lib.isString standalone.activationPackage.drvPath)
          true;
        assert expect "daemon ExecStart"
          (map (lib.hasSuffix "/bin/niri-dynamic-workspaces daemon")
            (lib.toList standalone.config.systemd.user.services.niri-dynamic-workspaces.Service.ExecStart))
          [ true ];
        assert expect "niri-flake binds"
          (bindKeys (withNiriFlake [ ])) [ "Mod+Ctrl+D" "Mod+D" "Mod+Shift+D" ];
        assert expect "null deleteKeybind"
          (bindKeys (withNiriFlake [{ programs.niri-dynamic-workspaces.deleteKeybind = null; }]))
          [ "Mod+D" "Mod+Shift+D" ];
        assert expect "all keybinds null"
          (withNiriFlake [ noKeybinds ]).programs.niri.settings
          null;
        assert expect "Home Manager niri module"
          (withNiriFlake [{ wayland.windowManager.niri.enable = true; }]).programs.niri.settings
          null;
        pkgs.emptyFile;
    };
}
