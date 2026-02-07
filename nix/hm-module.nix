{ self, inputs, ... }:
{
  imports = [
    inputs.home-manager.flakeModules.home-manager
  ];

  flake.homeModules.default = { pkgs, lib, config, ... }:
    let
      cfg = config.programs.niri-dynamic-workspaces;
      tomlFormat = pkgs.formats.toml { };
    in
    {
      options.programs.niri-dynamic-workspaces = {
        enable = lib.mkEnableOption "niri-dynamic-workspaces";

        package = lib.mkOption {
          type = lib.types.package;
          default = self.packages.${pkgs.system}.default;
          description = "The niri-dynamic-workspaces package to use.";
        };

        keybind = lib.mkOption {
          type = lib.types.str;
          default = "Mod+D";
          description = "Keybind to spawn niri-dynamic-workspaces.";
        };

        settings = lib.mkOption {
          type = tomlFormat.type;
          default = { };
          description = ''
            Configuration written to
            {file}`$XDG_CONFIG_HOME/niri-dynamic-workspaces/config.toml`.

            See the available options in README.md
            ```
          '';
          example = lib.literalExpression ''
            {
              general.workspace_prefix = "ws-";
              layout.max_columns = 3;
              keybinds.delete_modifier = "Alt";
            }
          '';
        };
      };

      config = lib.mkIf cfg.enable {
        home.packages = [ cfg.package ];

        programs.niri.settings.binds."${cfg.keybind}".action.spawn =
          [ "${cfg.package}/bin/niri-dynamic-workspaces" ];

        xdg.configFile."niri-dynamic-workspaces/config.toml" =
          lib.mkIf (cfg.settings != { }) {
            source = tomlFormat.generate "config.toml" cfg.settings;
          };
      };
    };
}
