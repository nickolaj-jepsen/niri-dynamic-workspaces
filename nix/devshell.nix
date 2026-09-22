{ self, ... }:
{
  perSystem = { pkgs, system, ... }: {
    devShells.default = pkgs.mkShell {
      inputsFrom = [
        self.packages.${system}.default
      ];

      nativeBuildInputs = [
        pkgs.rustc
        pkgs.cargo
        pkgs.clippy
        pkgs.rustfmt

        # e2e harness: nested compositor, input injection, screenshots
        pkgs.niri
        pkgs.cage
        pkgs.wtype
        pkgs.grim
        pkgs.wlrctl
        pkgs.jq
        pkgs.dbus
        pkgs.foot

        # e2e/render-readme.sh: a bigger output, and a GUI app behind the overlay
        pkgs.wlr-randr
        pkgs.gnome-text-editor
      ];

      # e2e/harness.sh hands this to the nested niri only; exporting the libglvnd variable would override host GL.
      NDW_E2E_EGL_VENDOR = "${pkgs.mesa}/share/glvnd/egl_vendor.d/50_mesa.json";
    };
  };
}
