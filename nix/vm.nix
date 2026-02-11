{ self, inputs, ... }:
{
  perSystem = { system, ... }:
    let
      nixos = inputs.nixpkgs.lib.nixosSystem {
        inherit system;
        modules = [
          "${inputs.nixpkgs}/nixos/modules/virtualisation/qemu-vm.nix"
          inputs.home-manager.nixosModules.home-manager

          ({ pkgs, ... }: {
            # VM hardware
            virtualisation = {
              memorySize = 2048;
              cores = 2;
              qemu.options = [
                "-device" "virtio-vga-gl"
                "-display" "gtk,gl=on"
              ];
            };

            # Compositor
            programs.niri.enable = true;

            # Auto-start niri for alice via greetd
            services.greetd = {
              enable = true;
              settings.default_session = {
                command = "niri-session";
                user = "alice";
              };
            };

            # Convenience packages
            environment.systemPackages = [ pkgs.foot ];

            # User
            users.users.alice = {
              isNormalUser = true;
              extraGroups = [ "wheel" "video" ];
              password = "alice";
            };
            security.sudo.wheelNeedsPassword = false;

            # Home Manager for alice
            home-manager = {
              useGlobalPkgs = true;
              useUserPackages = true;
              users.alice = { ... }: {
                imports = [
                  self.homeModules.default
                  inputs.niri-flake.homeModules.config
                ];

                programs.niri-dynamic-workspaces = {
                  enable = true;
                  keybind = "Alt+D";
                  deleteKeybind = "Alt+Ctrl+D";
                  moveWindowKeybind = "Alt+Shift+D";
                };

                home.stateVersion = "24.11";
              };
            };

            system.stateVersion = "24.11";
          })
        ];
      };
    in
    {
      packages.vm = nixos.config.system.build.vm;
    };
}
