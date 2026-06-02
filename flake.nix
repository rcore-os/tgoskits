{
  description = "TGOSKits development shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      perSystem =
        { system, ... }:
        let
          pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [
              # (import inputs.rust-overlay)
            ];
          };

          lib = pkgs.lib;

          optionalPackageByPath =
            path:
            let
              package = lib.attrByPath path null pkgs;
            in
            lib.optional (package != null) package;

          llvmPackages = pkgs.llvmPackages;
          rustBin = inputs.rust-overlay.lib.mkRustBin {
            distRoot = "https://mirrors.ustc.edu.cn/rust-static";
          } pkgs;
          rustToolchain = rustBin.fromRustupToolchainFile ./rust-toolchain.toml;

          commonPackages =
            with pkgs;
            [
              bashInteractive
              binutils
              bzip2
              cacert
              clang
              cmake
              curl
              dosfstools
              e2fsprogs
              file
              git
              gnumake
              meson
              ninja
              openssl
              pkg-config
              python3
              qemu
              rustToolchain
              # rustup
              wget
              xz
              zlib
            ]
            ++ [
              llvmPackages.bintools
              llvmPackages.libclang
              llvmPackages.llvm
            ]
            ++ optionalPackageByPath [ "cargo-binutils" ]
            ++ optionalPackageByPath [ "dtc" ]
            ++ optionalPackageByPath [ "glib" ]
            ++ optionalPackageByPath [ "libpixman" ]
            ++ optionalPackageByPath [ "libslirp" ]
            ++ optionalPackageByPath [ "libudev-zero" ]
            ++ optionalPackageByPath [ "mtools" ]
            ++ optionalPackageByPath [ "qemu-user" ]
            ++ optionalPackageByPath [ "qemu-user-static" ]
            ++ optionalPackageByPath [ "xorriso" ];

          crossPackages =
            optionalPackageByPath [
              "pkgsCross"
              "aarch64-multiplatform-musl"
              "stdenv"
              "cc"
            ]
            ++ optionalPackageByPath [
              "pkgsCross"
              "musl64"
              "stdenv"
              "cc"
            ]
            ++ optionalPackageByPath [
              "pkgsCross"
              "riscv64"
              "stdenv"
              "cc"
            ]
            ++ optionalPackageByPath [
              "pkgsCross"
              "loongarch64-linux"
              "stdenv"
              "cc"
            ];

          mkTgoskitsShell =
            {
              name,
              extraPackages ? [ ],
            }:
            pkgs.mkShell {
              inherit name;

              packages = commonPackages ++ extraPackages;

              LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";

              shellHook = ''
                project_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

                # export RUSTUP_HOME="$project_root/.rustup"
                export CARGO_HOME="$project_root/.cargo"
                export PATH="$CARGO_HOME/bin:$PATH"

                # mkdir -p "$RUSTUP_HOME" "$CARGO_HOME" "$CARGO_HOME/bin"
                mkdir -p "$CARGO_HOME" "$CARGO_HOME/bin"

                echo "TGOSKits dev shell"
                # echo "  RUSTUP_HOME=$RUSTUP_HOME"
                echo "  CARGO_HOME=$CARGO_HOME"
                echo "  Rust toolchain: rust-overlay from rust-toolchain.toml"

                exec fish
              '';
            };
        in
        {
          devShells.default = mkTgoskitsShell {
            name = "tgoskits-dev";
          };

          devShells.full = mkTgoskitsShell {
            name = "tgoskits-dev-full";
            extraPackages = crossPackages;
          };

          formatter = pkgs.nixfmt;
        };
    };
}
