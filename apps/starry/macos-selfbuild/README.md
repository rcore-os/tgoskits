# StarryOS macOS AArch64 Self-Build

This app reproduces a StarryOS self-build on Apple Silicon macOS. The host
builds a seed AArch64 StarryOS kernel, boots it with QEMU HVF, and the StarryOS
guest runs Cargo to build `starryos` again. The guest-built kernel is copied
back out of the work rootfs and can be booted again with QEMU.

The app keeps the special macOS/HVF pieces local to `apps/starry/macos-selfbuild`.
The only platform-facing requirements are generic AArch64 boot arguments:

```text
someboot.aarch64_timer=virtual someboot.aarch64_gicd_spi=off
```

Without those arguments, AArch64 keeps the normal EL1 CNTP/physical timer path
and the normal GICv3 distributor initialization.

## What This Flow Does

`reproduce.sh` runs the full default workflow:

1. builds the seed StarryOS AArch64 kernel with `cargo xtask starry build`;
2. pulls the managed AArch64 Alpine rootfs with `cargo xtask image pull`;
3. expands that managed rootfs with `cargo xtask image resize`;
4. prepares an app-local toolchain overlay for the guest;
5. copies the managed rootfs to a per-run work image;
6. injects the app overlay into that copied work image;
7. boots QEMU/HVF without `-snapshot`;
8. runs guest Cargo directly inside StarryOS;
9. refreshes kallsyms and writes the guest-built kernel into the work rootfs;
10. extracts the ELF and `.bin` from the work rootfs with `debugfs`.

The managed rootfs lives under:

```text
target/starry-macos-selfbuild/tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img
```

It is not under `tmp/axbuild/rootfs`. The managed image is kept clean; only the
per-run copy under `target/starry-macos-selfbuild/rootfs/` is patched.

## Prerequisites

Install the host tools on Apple Silicon macOS:

```bash
brew install qemu e2fsprogs zig llvm
```

The first run also needs network access for the managed rootfs, Alpine APKs,
Rust dist components, and Cargo registry archives required by `Cargo.lock`.
After the overlay cache is prepared, guest Cargo runs offline.

## Full Reproduction

From the repository root:

```bash
apps/starry/macos-selfbuild/reproduce.sh
```

Successful self-build output contains:

```text
===STARRY-MACOS-SELFBUILD-PASS jobs=4 elapsed=<seconds>===
```

The extracted artifacts are:

```text
target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat
target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat.bin
```

## Boot The Guest-Built Kernel

After the full reproduction succeeds:

```bash
BOOT_ONLY=1 \
PREPARE_OVERLAY=0 REQUIRE_FRESH_ROOTFS=0 \
KERNEL=target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat.bin \
ROOTFS=target/starry-macos-selfbuild/tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img \
SMP=4 JOBS=4 MEM=8192M QEMU_NET=0 QEMU_TIMEOUT_SEC=300 \
CASE_NAME=selfbuilt-boot-verify \
apps/starry/macos-selfbuild/run_selfbuild.sh
```

Successful boot-only verification reaches the StarryOS shell and writes:

```text
root@starry:
===HOST-QEMU-STOP reason=boot-only-shell ... rc=0===
```

## Reusing Prepared Inputs

Reuse the current rootfs and toolchain overlay, but rerun QEMU:

```bash
ROOTFS_MODE=skip apps/starry/macos-selfbuild/reproduce.sh
```

Prepare or refresh only the rootfs inputs:

```bash
apps/starry/macos-selfbuild/build_rootfs.sh
```

Force rebuilding the guest toolchain overlay:

```bash
apps/starry/macos-selfbuild/build_rootfs.sh --force-toolchain
```

## Toolchain Overlay

The overlay is a filesystem tree, not a rootfs image:

```text
target/starry-macos-selfbuild/rootfs-build/toolchain-overlay
```

It is prepared from Alpine AArch64 APKs and official Rust dist components. It
contains the guest Rust/Cargo tools, Rust source, LLVM/libclang, musl C tools,
and an offline Cargo registry cache. The app injects this tree into the copied
work rootfs before QEMU starts.

## Guest Cargo Build

The guest build is a direct Cargo build of StarryOS:

```text
cargo build -p starryos \
  --target apps/starry/macos-selfbuild/target-aarch64-unknown-none-softfloat-pie.json \
  -Z json-target-spec -Z host-config -Z target-applies-to-host \
  --bin starryos \
  -Z build-std=core,alloc \
  -Z build-std-features=compiler-builtins-mem \
  --features plat-dyn,ax-driver/virtio-blk,ax-driver/virtio-net,smp \
  --release
```

The flow does not pass `--no-default-features` and does not enforce a crate-count
limit. A validated no-pregenerated-bindings run on 2026-06-21 built `420/420`
Cargo units. The current StarryOS dependency graph keeps `membarrier`, kallsyms,
and `kprobe` available.

## Bindgen And libclang

The app does not inject pregenerated Rust binding files. Guest build scripts run
normally, including raw `bindgen` for crates such as `ax-posix-api` and
`lwprintf-rs`.

The previous `libclang.so ... Dynamic loading not supported` failure came from
host build-script artifacts being built as static musl binaries. The guest
wrapper writes this Cargo home configuration:

```toml
[host]
rustflags = ["-C", "target-feature=-crt-static"]
```

Cargo is then invoked with `-Z host-config -Z target-applies-to-host`, so host
build scripts can dynamically load libclang while the StarryOS target remains
the custom AArch64 PIE target.

## Important Knobs

| Variable | Default | Meaning |
| --- | --- | --- |
| `ROOTFS_MODE` | `build-rootfs` | Use `skip` to reuse prepared rootfs inputs. |
| `ROOTFS_SIZE_MIB` | `16384` | Managed rootfs size after `cargo xtask image resize`. |
| `TGOS_IMAGE_LOCAL_STORAGE` | `target/starry-macos-selfbuild/tgos-images` | xtask image storage root. |
| `SMP` | `4` | QEMU vCPU count. |
| `JOBS` | `$SMP` | Guest Cargo jobs. |
| `MEM` | `8192M` | QEMU memory size. |
| `QEMU_APPEND` | `someboot.aarch64_timer=virtual someboot.aarch64_gicd_spi=off` | Generic AArch64 platform boot arguments for macOS/HVF. |
| `QEMU_SNAPSHOT` | `0` | Must stay `0` for self-build artifact extraction. |
| `PREPARE_OVERLAY` | `1` | Build and inject the app overlay into the copied work rootfs. |
| `ARTIFACT_EXTRACT` | `1` | Extract guest-built artifacts from the work rootfs after QEMU exits. |
| `ARTIFACT_OUT_DIR` | `target/starry-macos-selfbuild/uploaded` | Host output directory for extracted kernels. |
| `STARRY_KALLSYMS_RESERVED` | `16M` | Temporary linker reserve used before guest kallsyms refresh. |

## Logs And Report

Per-run logs are written under:

```text
target/starry-macos-selfbuild/logs/
target/starry-macos-selfbuild/work/
```

The development report for this branch is maintained at:

```text
tmp/macos-selfbuild-report.md
```

## Maintenance Notes

- Keep app-specific rootfs preparation, QEMU wrapping, and artifact extraction
  inside `apps/starry/macos-selfbuild`.
- Keep apps outside changes generic: rootfs resize is an xtask image operation,
  timer selection is a generic AArch64 boot argument, and GICD SPI access is a
  generic GIC capability switch.
- Do not add app-private Cargo features for timer or GIC behavior.
- Do not inject pregenerated binding sources; fix the guest build environment
  instead.
