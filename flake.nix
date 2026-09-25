{
  description = "Carosello — a fast, minimalist image and video viewer for Linux";

  inputs = {
    # The only input, bumped weekly by .github/workflows/update-flake-lock.yml.
    # No per-release maintenance: the package builds this flake's own source
    # tree and reads its version from Cargo.toml (the single source of truth).
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        rec {
          carosello = pkgs.callPackage ./nix/package.nix {
            src = ./.;
            inherit version;
          };
          default = carosello;
        });

      # `nix flake check` builds the package, so a stale/broken flake fails CI.
      checks = forAllSystems (system: {
        inherit (self.packages.${system}) carosello;
      });
    };
}
