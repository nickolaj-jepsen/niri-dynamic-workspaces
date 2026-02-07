{ self, ... }:
let
  cargoConfig = self + "/Cargo.toml";
  cargoLock = self + "/Cargo.lock";
  cargoToml = builtins.fromTOML (builtins.readFile cargoConfig);
in
{
  perSystem = { pkgs, lib, ... }: {
    packages.default = pkgs.rustPlatform.buildRustPackage {
      inherit (cargoToml.package) version;
      pname = cargoToml.package.name;

      src = lib.cleanSourceWith {
        src = self;
        filter = path: type:
          let
            name = baseNameOf path;
            relPath = lib.removePrefix (toString self + "/") (toString path);
          in
          lib.hasPrefix "src" relPath
          || name == "Cargo.toml"
          || name == "Cargo.lock"
          || name == "style.css";
      };

      cargoLock.lockFile = cargoLock;

      nativeBuildInputs = [
        pkgs.pkg-config
      ];

      buildInputs = [
        pkgs.gtk4
        pkgs.gtk4-layer-shell
        pkgs.glib
      ];
    };
  };
}
