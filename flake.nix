{
  description = "Themelio wallet daemon";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-20.09";
  inputs.flake-utils.url = "github:numtide/flake-utils";
  inputs.naersk.url = "github:nmattia/naersk";
  inputs.mozilla = { url = "github:mozilla/nixpkgs-mozilla"; flake = false; };

  outputs =
    { self
    , nixpkgs
    , mozilla
    , flake-utils
    , naersk
    , ...
    } @inputs:
    let rustOverlay = final: prev:
          let rustChannel = prev.rustChannelOf {
            channel = "1.52.0";
            sha256 = "sha256-fcaq7+4shIvAy0qMuC3nnYGd0ZikkR5ln/rAruHA6mM=";
          };
          in
          { inherit rustChannel;
            rustc = rustChannel.rust;
            cargo = rustChannel.rust;
          };

    #in flake-utils.lib.eachSystem
    #  ["x86_64-linux"]
    in flake-utils.lib.eachDefaultSystem
      (system: let

        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            (import "${mozilla}/rust-overlay.nix")
            rustOverlay
          ];
        };

        naersk-lib = naersk.lib."${system}";

        in rec {
          packages.melwalletd = naersk-lib.buildPackage rec {
            name = "melwalletd";
            #name = "melwalletd-v${version}";
            #version = "0.1.0-alpha";
            copyBins = true;
            root = ./.;
          };

          defaultPackage = packages.melwalletd;

          devShell = pkgs.mkShell {
            buildInputs = with pkgs; [
              (rustChannel.rust.override { extensions = [ "rust-src" ]; })
            ];
          };
        });
}
