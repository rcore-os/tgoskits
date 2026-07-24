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
            # distRoot = "https://mirrors.ustc.edu.cn/rust-static";
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

          crossCompilers =
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

          mkCrossCompilerAliases =
            {
              name,
              packagePath,
              sourcePrefix,
              aliasPrefix,
            }:
            let
              crossCompiler = lib.attrByPath packagePath null pkgs;
              tools = [
                "ar"
                "c++"
                "cc"
                "cpp"
                "g++"
                "gcc"
                "ld"
                "nm"
                "objcopy"
                "objdump"
                "ranlib"
                "readelf"
                "strip"
              ];
              aliases = map (
                tool:
                pkgs.writeShellScriptBin "${aliasPrefix}-${tool}" ''
                  exec "${crossCompiler}/bin/${sourcePrefix}-${tool}" \
                    ${
                      lib.optionalString (builtins.elem tool [
                        "c++"
                        "cc"
                        "g++"
                        "gcc"
                      ]) "-fno-stack-protector"
                    } "$@"
                ''
              ) tools;
            in
            lib.optional (crossCompiler != null) (
              pkgs.symlinkJoin {
                inherit name;
                paths = aliases;
              }
            );

          crossCompilerAliases =
            mkCrossCompilerAliases {
              name = "x86_64-linux-musl-toolchain-aliases";
              packagePath = [
                "pkgsCross"
                "musl64"
                "stdenv"
                "cc"
              ];
              sourcePrefix = "x86_64-unknown-linux-musl";
              aliasPrefix = "x86_64-linux-musl";
            }
            ++ mkCrossCompilerAliases {
              name = "aarch64-linux-musl-toolchain-aliases";
              packagePath = [
                "pkgsCross"
                "aarch64-multiplatform-musl"
                "stdenv"
                "cc"
              ];
              sourcePrefix = "aarch64-unknown-linux-musl";
              aliasPrefix = "aarch64-linux-musl";
            };

          # Keep cross compilers out of `packages`: their setup hooks would otherwise
          # override host compiler variables used by native Rust builds.
          crossCompilerPath = lib.makeBinPath (crossCompilerAliases ++ crossCompilers);
        in
        {
          devShells.default = pkgs.mkShell {
            name = "tgoskits-dev";

            packages = commonPackages;

            LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";

            shellHook = ''
              export project_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

              # export RUSTUP_HOME="$project_root/.rustup"
              export CARGO_HOME="$project_root/.cargo"
              export PATH="$CARGO_HOME/bin:${crossCompilerPath}:$PATH"

              unset CC CXX AR RANLIB
              unset CC_x86_64_unknown_linux_gnu
              unset CXX_x86_64_unknown_linux_gnu
              unset AR_x86_64_unknown_linux_gnu
              unset RANLIB_x86_64_unknown_linux_gnu

              # mkdir -p "$RUSTUP_HOME" "$CARGO_HOME" "$CARGO_HOME/bin"
              mkdir -p "$CARGO_HOME" "$CARGO_HOME/bin"

              echo "TGOSKits dev shell"
              # echo "  RUSTUP_HOME=$RUSTUP_HOME"
              echo "  CARGO_HOME=$CARGO_HOME"
              echo "  Rust toolchain: rust-overlay from rust-toolchain.toml"
              echo "  Cross compilers: available by target-prefixed command name"
            '';
          };

          formatter = pkgs.nixfmt;
        };
    };
}
