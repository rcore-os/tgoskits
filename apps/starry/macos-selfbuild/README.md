# StarryOS macOS AArch64 Self-Build

This app is the Apple Silicon macOS self-build workflow for StarryOS. The host
builds a seed AArch64 StarryOS kernel, boots it with QEMU AArch64 TCG, and the
StarryOS guest then runs Cargo to build `starryos` again. The default path does
not use HVF and does not enable the `cntv-timer` or `qemu-hvf-gic` feature
plumbing.

The rootfs image is managed by xtask image storage. The default path is:

```text
target/starry-macos-selfbuild/tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img
```

It is not under `tmp/axbuild/rootfs`. xtask still uses
`tmp/axbuild/starry-app/macos-selfbuild/` as temporary staging while injecting
the app overlay.

## Prerequisites

```bash
brew install qemu e2fsprogs zig llvm
```

The workflow uses `qemu-system-aarch64`, Homebrew `e2fsprogs`, Zig or an
existing `aarch64-linux-musl-*` toolchain for the seed build, and LLVM binutils
for kallsyms/bin refresh.

If direct network access is unstable, run through the local proxy helper when it
is available:

```bash
set_proxy apps/starry/macos-selfbuild/reproduce.sh
```

If `set_proxy` is not defined in the shell, set proxy variables explicitly:

```bash
HTTP_PROXY=http://127.0.0.1:9567 \
HTTPS_PROXY=http://127.0.0.1:9567 \
ALL_PROXY=http://127.0.0.1:9567 \
apps/starry/macos-selfbuild/reproduce.sh
```

## Fresh Reproduction

From a fresh checkout on Apple Silicon macOS:

```bash
apps/starry/macos-selfbuild/reproduce.sh
```

The script performs the whole default flow:

1. builds the seed StarryOS kernel and host-generated bindings;
2. pulls the managed AArch64 Alpine rootfs through `cargo xtask image pull`;
3. expands it with `cargo xtask image resize` to `ROOTFS_SIZE_MIB` MiB
   (default `16384`);
4. prepares the app-local guest toolchain overlay under
   `target/starry-macos-selfbuild/rootfs-build/toolchain-overlay`;
5. runs `cargo xtask starry app qemu -t macos-selfbuild --arch aarch64`;
6. lets xtask run `prebuild.sh` and inject the overlay through the existing
   Starry app `inject_overlay` path;
7. keeps the rootfs persistent for this app run and extracts the guest-built
   kernel artifacts from `/opt/starryos-selfbuild-artifacts` with `debugfs`.

The expected self-build success marker is:

```text
===STARRY-MACOS-SELFBUILD-PASS jobs=4 elapsed=<seconds>===
```

The extracted artifacts are written to:

```text
target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat
target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat.bin
```

## Boot The Self-Built Kernel

After `reproduce.sh` succeeds, boot the extracted guest-built kernel:

```bash
BOOT_ONLY=1 \
KERNEL=target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat.bin \
ROOTFS=target/starry-macos-selfbuild/tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img \
SMP=4 JOBS=4 MEM=3072M QEMU_NET=0 QEMU_TIMEOUT_SEC=300 \
CASE_NAME=selfbuilt-boot-verify \
apps/starry/macos-selfbuild/run_selfbuild.sh
```

A successful boot reaches the StarryOS shell and writes:

```text
root@starry:
===HOST-QEMU-STOP reason=boot-only-shell ... rc=0===
```

`run_selfbuild.sh` is kept as the app-local boot verification wrapper for an
already produced kernel. The rootfs preparation and self-build launch path above
uses xtask for image pull/resize, overlay injection, and QEMU app execution.

## Reuse Existing Inputs

To reuse the current rootfs/toolchain overlay and only rerun injection/QEMU:

```bash
ROOTFS_MODE=skip apps/starry/macos-selfbuild/reproduce.sh
```

To rebuild only the managed rootfs inputs:

```bash
apps/starry/macos-selfbuild/build_rootfs.sh
```

`build_rootfs.sh` does not inject files manually. It pulls/resizes the managed
base image and prepares the app-local toolchain overlay cache. The actual rootfs
injection happens when `cargo xtask starry app qemu` runs this app's
`prebuild.sh`.

## Toolchain Overlay

The app-local toolchain overlay is built from Alpine AArch64 APKs and official
Rust dist components. It contains the guest Rust/Cargo tools, Rust source,
libclang/LLVM bits, musl C tools, and an offline Cargo registry cache. It is a
filesystem tree, not a rootfs image.

Useful paths:

```text
target/starry-macos-selfbuild/rootfs-build/toolchain-overlay
target/starry-macos-selfbuild/rootfs-build/cargo-home
```

Force rebuilding it with:

```bash
apps/starry/macos-selfbuild/build_rootfs.sh --force-toolchain
```

## Artifact Return

The default path does not use a host upload service. This app's QEMU config sets
`persist_rootfs = true`, so xtask does not add `-snapshot` for the app run. After
the guest prints the PASS marker, `reproduce.sh` runs `e2fsck` on the managed
rootfs and extracts:

```text
/opt/starryos-selfbuild-artifacts/starryos-aarch64-unknown-none-softfloat
/opt/starryos-selfbuild-artifacts/starryos-aarch64-unknown-none-softfloat.bin
```

The optional Python receiver exists only as a fallback for manual experiments
that need guest-to-host network upload. It is not used for the default rootfs
build, overlay injection, QEMU launch, or artifact return path.

Knobs:

| Variable | Default | Meaning |
| --- | --- | --- |
| `ARTIFACT_EXTRACT` | `1` | Extract guest-built artifacts from the persistent rootfs after QEMU exits. |
| `ARTIFACT_UPLOAD` | `0` | Start the optional host artifact receiver. |
| `ARTIFACT_UPLOAD_PORT` | `18180` | Receiver port visible to the guest as `10.0.2.2:<port>`. |
| `ARTIFACT_UPLOAD_DIR` | `target/starry-macos-selfbuild/uploaded` | Host output directory. |
| `ARTIFACT_UPLOAD_REQUIRED` | `1` | Fail the guest run if upload fails. |

## Build Profile

The default guest build profile is intentionally narrow:

```text
features=plat-dyn,ax-driver/virtio-blk,smp
rustc_threads=2
expected_crates~420
```

The profile keeps `membarrier`, `kallsyms`, and `kprobe` available through the
current StarryOS configuration. It does not enable the old broad driver/device
feature set for this app.

## Important Knobs

| Variable | Default | Meaning |
| --- | --- | --- |
| `ROOTFS_MODE` | `build-rootfs` | Use `skip` to reuse current rootfs inputs. |
| `ROOTFS_SIZE_MIB` | `16384` | Managed image size after `cargo xtask image resize`. |
| `TGOS_IMAGE_LOCAL_STORAGE` | `target/starry-macos-selfbuild/tgos-images` | xtask image storage root. |
| `JOBS` | `4` | Guest Cargo jobs. |
| `RUSTC_THREADS` | `2` | Guest `-Zthreads` value. |
| `SOURCE_TMPFS` | `1` | Copy source to guest `/tmp` before building. |
| `GUEST_MONITOR_INTERVAL_SEC` | `60` | Guest process snapshots while Cargo runs. |
| `STARRY_KALLSYMS_RESERVED` | `16M` | Reserved `.kallsyms` section space. |

## Sanity Checks

Check shell/Python syntax after edits:

```bash
bash -n apps/starry/macos-selfbuild/*.sh
python3 -m py_compile apps/starry/macos-selfbuild/artifact_upload_server.py
```

Check the prepared managed rootfs has received the app overlay after a qemu app
run:

```bash
apps/starry/macos-selfbuild/check_rootfs.sh \
  target/starry-macos-selfbuild/tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img
```
