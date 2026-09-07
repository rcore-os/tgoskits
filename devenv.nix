{
  pkgs,
  lib,
  config,
  ...
}:

let
  # Wrapper scripts only. Real pkgsCross.cc stays off `packages` so its
  # setup hooks cannot override host CC for native Rust builds.
  mkMuslAliases =
    {
      name,
      cc,
      sourcePrefix,
      aliasPrefix,
    }:
    let
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
    in
    pkgs.symlinkJoin {
      inherit name;
      paths = map (
        tool:
        pkgs.writeShellScriptBin "${aliasPrefix}-${tool}" ''
          exec "${cc}/bin/${sourcePrefix}-${tool}" \
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
    };
in
{
  name = "tgoskits-dev";

  languages.rust = {
    enable = true;
    toolchainFile = ./rust-toolchain.toml;
    # Default Linux linker is clang; this repo unsets CC and lets rustc pick.
    clangLinker.enable = false;
  };

  # rust.nix turns this on; we do not want a host cc leaking into cargo.
  languages.c.enable = false;

  packages = with pkgs; [
    binutils
    bzip2
    cacert
    cargo-binutils
    clang
    cmake
    curl
    dosfstools
    dtc
    e2fsprogs
    fakeroot
    git
    glib
    gnumake
    libslirp
    libudev-zero
    meson
    mtools
    ninja
    openssl
    pkg-config
    python3
    qemu
    qemu-user
    wget
    xorriso
    xz
    zlib
    llvmPackages.bintools
    llvmPackages.libclang
    llvmPackages.llvm
    (mkMuslAliases {
      name = "x86_64-linux-musl-toolchain-aliases";
      cc = pkgsCross.musl64.stdenv.cc;
      sourcePrefix = "x86_64-unknown-linux-musl";
      aliasPrefix = "x86_64-linux-musl";
    })
    (mkMuslAliases {
      name = "aarch64-linux-musl-toolchain-aliases";
      cc = pkgsCross.aarch64-multiplatform-musl.stdenv.cc;
      sourcePrefix = "aarch64-unknown-linux-musl";
      aliasPrefix = "aarch64-linux-musl";
    })
  ];

  env.LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

  enterShell =
    let
      rust = config.languages.rust.toolchainPackage;
      crossBins = lib.makeBinPath [
        pkgs.pkgsCross.aarch64-multiplatform-musl.stdenv.cc
        pkgs.pkgsCross.musl64.stdenv.cc
        pkgs.pkgsCross.riscv64.stdenv.cc
        pkgs.pkgsCross.loongarch64-linux.stdenv.cc
      ];
    in
    ''
      export project_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

      # In-tree crate cache. rust-toolchain.toml is a rustup override;
      # without this, /run/current-system/sw/bin/cargo compiles with
      # ~/.rustup and a GC'd Nix lld wrapper.
      export CARGO_HOME="$project_root/.cargo"
      mkdir -p "$CARGO_HOME" "$CARGO_HOME/bin"
      export CARGO="${rust}/bin/cargo"
      export RUSTC="${rust}/bin/rustc"
      unset RUSTUP_TOOLCHAIN
      export PATH="${rust}/bin:$CARGO_HOME/bin:${crossBins}:$PATH"
      hash -r 2>/dev/null || true

      unset CC CXX AR RANLIB
      unset CC_x86_64_unknown_linux_gnu
      unset CXX_x86_64_unknown_linux_gnu
      unset AR_x86_64_unknown_linux_gnu
      unset RANLIB_x86_64_unknown_linux_gnu

      echo "TGOSKits devenv"
      echo "  CARGO_HOME=$CARGO_HOME"
      echo "  CARGO=$CARGO"
      echo "  Rust toolchain: languages.rust from rust-toolchain.toml"
      echo "  Cross compilers: available by target-prefixed command name"
    '';
}
