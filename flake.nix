{
  description = "Themelio wallet daemon";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-20.09";
  inputs.flake-utils.url = "github:numtide/flake-utils";
  inputs.mozilla = { url = "github:mozilla/nixpkgs-mozilla"; flake = false; };
  inputs.fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    { self
    , nixpkgs
    , mozilla
    , flake-utils
    , fenix
    , ...
    } @inputs:

    flake-utils.lib.eachDefaultSystem
      (system: let

        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            (import "${mozilla}/rust-overlay.nix")
            fenix.overlay
          ];
        };

        rustPlatform = pkgs.makeRustPlatform {
          inherit (fenix.packages.${system}.minimal) cargo rustc;
        };

        cargoToml = (builtins.fromTOML (builtins.readFile ./Cargo.toml));

        in rec {
          packages.melwalletd = rustPlatform.buildRustPackage rec {
            pname = cargoToml.package.name;
            version = cargoToml.package.version;

            src = "${self}";

            cargoSha256 = "sha256-2yq0YlsGIRD+mFtJZnj2HkJoMz0iggaVGQoK0Yys7gc=";
          };

          defaultPackage = packages.melwalletd;

          devShell = pkgs.mkShell {
            buildInputs = with pkgs; [
              (rustChannel.rust.override {
                extensions = [ "rust-src" "-musl" ];
              })
            ];

            shellHook = ''
              export PATH=$PATH:${packages.melwalletd}/bin
            '';
          };
        });
}
