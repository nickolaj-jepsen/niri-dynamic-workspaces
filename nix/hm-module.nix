{ self, inputs, ... }:
{
  imports = [
    inputs.home-manager.flakeModules.home-manager
    ./hm-module-checks.nix
  ];

  flake.homeModules.default = { pkgs, lib, config, options, ... }:
    let
      cfg = config.programs.niri-dynamic-workspaces;
      tomlFormat = pkgs.formats.toml { };

      niriBinds = lib.listToAttrs (map
        ({ key, args, title }: lib.nameValuePair key {
          action.spawn = [ "${cfg.package}/bin/niri-dynamic-workspaces" ] ++ args;
          hotkey-overlay.title = title;
        })
        (lib.filter (b: b.key != null) [
          { key = cfg.keybind; args = [ ]; title = "Open Workspace Switcher"; }
          { key = cfg.deleteKeybind; args = [ "delete" ]; title = "Delete Workspace"; }
          { key = cfg.moveWindowKeybind; args = [ "move-window" ]; title = "Move Window to Workspace"; }
        ]));
    in
    {
      options.programs.niri-dynamic-workspaces = {
        enable = lib.mkEnableOption "niri-dynamic-workspaces";

        package = lib.mkOption {
          type = lib.types.package;
          default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          description = "The niri-dynamic-workspaces package to use.";
        };

        keybind = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = "Mod+D";
          description = ''
            Keybind to open the workspace switcher overlay, added to
            niri-flake's `programs.niri.settings.binds`. `null` leaves it out.
          '';
        };

        deleteKeybind = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = "Mod+Ctrl+D";
          description = ''
            Keybind to open the workspace delete overlay, added to
            niri-flake's `programs.niri.settings.binds`. `null` leaves it out.
          '';
        };

        moveWindowKeybind = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = "Mod+Shift+D";
          description = ''
            Keybind to open the move-window overlay, added to niri-flake's
            `programs.niri.settings.binds`. `null` leaves it out.
          '';
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

      config = lib.mkIf cfg.enable (lib.mkMerge [
        {
          home.packages = [ cfg.package ];

          systemd.user.services.niri-dynamic-workspaces = lib.mkIf cfg.daemon {
            Unit = {
              Description = "Niri dynamic workspaces daemon";
              PartOf = [ "graphical-session.target" ];
              After = [ "graphical-session.target" ];
              # Set by `niri --session`; keeps the daemon out of other sessions.
              ConditionEnvironment = "NIRI_SOCKET";
            };
            Service = {
              ExecStart = "${cfg.package}/bin/niri-dynamic-workspaces daemon";
              Restart = "on-failure";
              RestartSec = 5;
            };
            Install.WantedBy = [ "graphical-session.target" ];
          };

          programs.niri-dynamic-workspaces.settings.general.theme =
            lib.mkIf (cfg.themeCss != null) (lib.mkDefault "theme.css");

          xdg.configFile."niri-dynamic-workspaces/theme.css" =
            lib.mkIf (cfg.themeCss != null) { text = cfg.themeCss; };

          xdg.configFile."niri-dynamic-workspaces/config.toml" =
            lib.mkIf (cfg.settings != { }) {
              source = tomlFormat.generate "config.toml" cfg.settings;
            };
        }
        # programs.niri.settings exists only with niri-flake, and any definition makes it
        # generate config.kdl, which Home Manager's own niri module would also write.
        (lib.optionalAttrs (options ? programs.niri.settings) {
          programs.niri.settings =
            lib.mkIf (niriBinds != { } && !(config.wayland.windowManager.niri.enable or false)) {
              binds = niriBinds;
            };
        })
      ]);
    };
}
