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
          description = "Keybind to open the workspace switcher overlay.";
        };

        deleteKeybind = lib.mkOption {
          type = lib.types.str;
          default = "Mod+Ctrl+D";
          description = "Keybind to open the workspace delete overlay.";
        };

        moveWindowKeybind = lib.mkOption {
          type = lib.types.str;
          default = "Mod+Shift+D";
          description = "Keybind to open the move-window overlay.";
        };

        daemon = lib.mkOption {
          type = lib.types.bool;
          default = true;
          description = "Start the daemon at login for faster overlay display.";
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
            }
          '';
        };

        themeCss = lib.mkOption {
          type = lib.types.nullOr lib.types.lines;
          default = null;
          description = ''
            CSS written to
            {file}`$XDG_CONFIG_HOME/niri-dynamic-workspaces/theme.css` and
            selected as `settings.general.theme`. See Theming in README.md
            for the variables and classes.
          '';
          example = ''
            window {
              --bg: #1e1e2e;
              --fg: #cdd6f4;
              --accent: #89b4fa;
              --urgent: #f9e2af;
              --danger: #f38ba8;
            }
          '';
        };
      };

      config = lib.mkIf cfg.enable {
        home.packages = [ cfg.package ];

        systemd.user.services.niri-dynamic-workspaces = lib.mkIf cfg.daemon {
          Unit = {
            Description = "Niri dynamic workspaces daemon";
            PartOf = [ "graphical-session.target" ];
            After = [ "graphical-session.target" ];
          };
          Service = {
            ExecStart = "${cfg.package}/bin/niri-dynamic-workspaces daemon";
            Restart = "on-failure";
            RestartSec = 5;
          };
          Install.WantedBy = [ "graphical-session.target" ];
        };

        programs.niri.settings.binds = {
          "${cfg.keybind}" = {
            action.spawn =
              [ "${cfg.package}/bin/niri-dynamic-workspaces" ];
            hotkey-overlay.title = "Open Workspace Switcher";
          };
          "${cfg.deleteKeybind}" = {
            action.spawn =
              [ "${cfg.package}/bin/niri-dynamic-workspaces" "delete" ];
            hotkey-overlay.title = "Delete Workspace";
          };
          "${cfg.moveWindowKeybind}" = {
            action.spawn =
              [ "${cfg.package}/bin/niri-dynamic-workspaces" "move-window" ];
            hotkey-overlay.title = "Move Window to Workspace";
          };
        };

        programs.niri-dynamic-workspaces.settings.general.theme =
          lib.mkIf (cfg.themeCss != null) (lib.mkDefault "theme.css");

        xdg.configFile."niri-dynamic-workspaces/theme.css" =
          lib.mkIf (cfg.themeCss != null) { text = cfg.themeCss; };

        xdg.configFile."niri-dynamic-workspaces/config.toml" =
          lib.mkIf (cfg.settings != { }) {
            source = tomlFormat.generate "config.toml" cfg.settings;
          };
      };
    };
}
