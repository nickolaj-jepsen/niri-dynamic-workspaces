{
  # `nix flake check` builds this; buildRustPackage also runs cargo test.
  perSystem = { self', ... }: {
    checks.package = self'.packages.default;
  };
}
