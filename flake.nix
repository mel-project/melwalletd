{
  description = "Themelio wallet daemon";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-20.09";
  inputs.flake-utils.url = "github:numtide/flake-utils";
  inputs.naersk.url = "github:nmattia/naersk";
  inputs.mozilla = { url = "github:mozilla/nixpkgs-mozilla"; flake = false; };
  inputs.cargo2nix = { url = "github:cargo2nix/cargo2nix"; flake = false; };
  inputs.selfDir = { url = "path:."; flake = false; };

  outputs =
    { self
    , nixpkgs
    , mozilla
    , flake-utils
    , naersk
    , cargo2nix
    , selfDir
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

    #in flake-utils.lib.eachSystem
    #  ["x86_64-linux"]
    in flake-utils.lib.eachDefaultSystem
      (system: let

        #pkgs' = import nixpkgs { inherit system; };
        #cargo2nix = pkgs'.callPackage cargo2nixSrc {};
        cargo2nixOverlay = import "${cargo2nix}/overlay";

        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            (import "${mozilla}/rust-overlay.nix")
            rustOverlay
            cargo2nixOverlay
          ];
        };

        rustPlatform = let rustChannel = pkgs.rustChannelOf {
          channel = "1.52.0";
          sha256 = "sha256-fcaq7+4shIvAy0qMuC3nnYGd0ZikkR5ln/rAruHA6mM=";
        }; in
          pkgs.makeRustPlatform {
            cargo = rustChannel.rust;
            rustc = rustChannel.rust;
          };
        /*
        rustPkgs = pkgs.rustBuilder.makePackageSet' {
          rustChannel = "1.52.0";
          #sha256 = "sha256-fcaq7+4shIvAy0qMuC3nnYGd0ZikkR5ln/rAruHA6mM=";
          packageFun = import "${selfDir}/Cargo.nix";
        };
        */

        naersk-lib = naersk.lib."${system}";

        in rec {
          /*
          packages.melwalletd = naersk-lib.buildPackage rec {
            name = "melwalletd";
            #name = "melwalletd-v${version}";
            #version = "0.1.0-alpha";
            copyBins = true;
            root = ./.;
          };
          */
          #packages.melwalletd = rustPkgs.workspace.melwalletd {};
          packages.melwalletd = rustPlatform.buildRustPackage rec {
            pname = "melwalletd";
            version = "0.1.0-alpha";

            src = "${selfDir}";

            cargoSha256 = "sha256-3VyGVxJqIdz1RNfdIi492tWMaM1Kxn18uBSvhPLNBCw=";
          };

          defaultPackage = packages.melwalletd;

          devShell = pkgs.mkShell {
            buildInputs = with pkgs; [
              (rustChannel.rust.override { extensions = [ "rust-src" ]; })
            ];

            shellHook = ''
              export PATH=$PATH:${packages.melwalletd}/bin
            '';
          };
        });
}
