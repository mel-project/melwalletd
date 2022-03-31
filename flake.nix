{
  description = "Themelio wallet daemon";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-21.11";
  inputs.flake-utils.url = "github:numtide/flake-utils";
  inputs.fenix = {
    url = "github:nix-community/fenix";
    inputs.nixpkgs.follows = "nixpkgs";
  };
  inputs.naersk = {
    url = "github:nmattia/naersk";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    { self
    , nixpkgs
    , flake-utils
    , fenix
    , naersk
    , ...
    } @inputs:

    flake-utils.lib.eachDefaultSystem
      (system: let

        target = "x86_64-unknown-linux-musl";
        #target = "x86_64-unknown-linux-gnu";

        pkgs = import nixpkgs {
          inherit system;
        };

        # Rust toolchain
        toolchain = with fenix.packages.${system};
          combine [
            stable.rustc
            stable.cargo
            #targets.${target}.stable.rust-std
          ];

        # To read melwalletd project metadata
        cargoToml = (builtins.fromTOML (builtins.readFile ./Cargo.toml));

        in rec {
          # Build melwalletd with musl
          packages.melwalletd = (naersk.lib.${system}.override {
            cargo = toolchain;
            rustc = toolchain;
          }).buildPackage {
            pname = "melwalletd";
            root = ./.;
            #CARGO_BUILD_TARGET = target;
            #CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER =
              #"${pkgs.pkgsCross.aarch64-multiplatform.stdenv.cc}/bin/${target}-gcc";
          };

          defaultPackage = packages.melwalletd;

          devShell = pkgs.mkShell {
            buildInputs = with pkgs; [ toolchain ];

            shellHook = ''
              export PATH=$PATH:${packages.melwalletd}/bin
            '';
          };
        });
}
