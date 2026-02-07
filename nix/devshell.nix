{ self, ... }:
{
  perSystem = { pkgs, ... }: {
    devShells.default = pkgs.mkShell {
      inputsFrom = [
        self.packages.${pkgs.system}.default
      ];

      nativeBuildInputs = [
        pkgs.rustc
        pkgs.cargo
        pkgs.clippy
        pkgs.rustfmt
      ];
    };
  };
}
