{
  description = "Themelio wallet daemon";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-20.09";
  inputs.flake-utils.url = "github:numtide/flake-utils";
  inputs.mozilla = { url = "github:mozilla/nixpkgs-mozilla"; flake = false; };

  outputs =
    { self
    , nixpkgs
    , mozilla
    , flake-utils
    , ...
    } @inputs:
    let
      rustOverlay = final: prev:
        let rustChannel = prev.rustChannelOf {
          channel = "1.52.0";
          sha256 = "sha256-fcaq7+4shIvAy0qMuC3nnYGd0ZikkR5ln/rAruHA6mM=";
          #channel = "nightly";
          #sha256 = "sha256-yvUmasDp4hTmipedyiWEjFCAsZHuIiODCygBfdrTeqs";
        };
        in { inherit rustChannel;
          rustc = rustChannel.rust;
          cargo = rustChannel.rust;
      };

    #in flake-utils.lib.eachDefaultSystem
    in flake-utils.lib.eachSystem
    [ "x86_64-linux" ]
      (system: let

        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            (import "${mozilla}/rust-overlay.nix")
            rustOverlay
          ];
        };

        rust = pkgs.rustc.override {
          targets = [ "x86_64-unknown-linux-musl" ];
        };
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rust;
          rustc = rust;
        };

        cargoToml = (builtins.fromTOML (builtins.readFile ./Cargo.toml));

        in rec {
          packages.melwalletd = rustPlatform.buildRustPackage rec {
            pname = cargoToml.package.name;
            version = cargoToml.package.version;
            RUSTFLAGS = "--target sdf";

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
