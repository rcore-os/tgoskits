# Starry Apps

`apps/starry/` contains runnable StarryOS scenarios. Most direct child
directories are board cases selected by `cargo xtask starry app board -t <case>`;
some x86_64 QEMU demos provide their own `cargo xtask starry qemu` commands.

Cases are intentionally separate from `test-suit/starryos`: apps are
operator-facing workflows, while the test suit remains CI-oriented coverage.

## Case Layout

```text
apps/starry/<case>/
  init.sh
  build-<target>.toml
  board-<board>.toml
  <optional user projects>
```

- `init.sh` is read by `cargo xtask starry app board` and sent to the Starry shell
  as the startup command.
- `build-<target>.toml` is the StarryOS build config. It must either include a
  top-level `target = "..."` or encode the target in the filename.
- `board-<board>.toml` is the ostool board run config. It supplies the board
  type, shell prefix, success/failure regexes, timeout, and optional server
  defaults.
- User programs under the case are examples only. The board rootfs must already
  contain the program and its shared libraries unless the case says otherwise.

Example:

```bash
cargo xtask starry app board -t orangepi-5-plus-uvc
```

## Resource Monitor

The `resource-monitor` case provides an offline user-space collector and a static
viewer for StarryOS application experiments. It samples existing `/proc` files
into CSV/JSONL logs and replays StarryOS/Linux runs locally in the browser; it
does not add kernel counters, drivers, online telemetry, or robot workload
control.

```bash
cd apps/starry/resource-monitor/offline-viewer
python3 -m http.server 8000
```

See `resource-monitor/README.md` for the demo usage, log export flow, and file format.

## PicoClaw CLI

The `picoclaw-cli` case is an opt-in StarryOS x86_64 QEMU workflow for checking
PicoClaw compatibility in three stages: offline CLI smoke, online agent request,
and gateway service smoke. It also provides an interactive StarryOS shell for
manual PicoClaw use. It prepares local-only release assets and rootfs images
under `target/picoclaw/` and `tmp/axbuild/rootfs/`.

```bash
apps/starry/picoclaw-cli/prepare_picoclaw_rootfs.sh
cargo xtask starry qemu \
  --arch x86_64 \
  --qemu-config apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-offline.toml \
  --rootfs tmp/axbuild/rootfs/rootfs-x86_64-picoclaw.img
```

See `picoclaw-cli/README.md` for the online agent, gateway, and interactive
flows.

## K230 KPU NNCase

The `k230-kpu-nncase` case is the operator-facing K230 KPU/NPU demo. It installs
the StarryOS guest NNCase runtime demo binaries, `yolov8n_320.kmodel`, and
`bus.jpg` into the K230 rootfs overlay, then runs:

```text
.kmodel -> NNCase runtime -> KPU command stream -> /dev/kpu -> IRQ/done -> output hashes
```

```bash
bash apps/starry/k230-kpu-nncase/c/tools/build-nncase-runtime-binaries.sh
PATH="$PWD/target/qemu-k230-docker-build:$PATH" \
  cargo xtask starry app qemu -t k230-kpu-nncase --arch riscv64
```

See `k230-kpu-nncase/README.md` and `docs/k230-kpu-nncase-runtime.md` for the
asset preparation flow.

## macOS AArch64 Self-Build

The `macos-selfbuild` case is an Apple Silicon macOS workflow that boots an
AArch64 StarryOS SMP kernel with QEMU HVF, enters the StarryOS guest userland,
and runs guest `cargo build` to build StarryOS again.

```bash
apps/starry/macos-selfbuild/full_self_build.sh
qemu-system-aarch64 \
  -snapshot \
  -machine virt,gic-version=3 \
  -nographic \
  -cpu cortex-a53 \
  -m 512M \
  -smp 1 \
  -device virtio-blk-pci,drive=disk0 \
  -drive id=disk0,if=none,format=raw,file=tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img,file.locking=off \
  -kernel target/starry-macos-selfbuild/uploaded/starryos-aarch64-unknown-none-softfloat.bin \
  -netdev user,id=net0
```

`full_self_build.sh` is the default full entrypoint. It prepares host tools,
uses `cargo xtask starry app qemu -t macos-selfbuild --arch aarch64` for the
seed kernel build, rootfs preparation, overlay injection, and QEMU/HVF run, then
extracts the guest-built kernel into `target/starry-macos-selfbuild/uploaded/`.
See `macos-selfbuild/README.md` and `macos-selfbuild/README_CN.md` for the script
roles, M3 validation environment, per-stage timing, rootfs path, PASS markers,
and direct QEMU boot verification of the self-built kernel.

## Redis

The `redis` case is a QEMU app workflow that installs Redis into a temporary
Alpine staging root and injects the Redis binaries, scripts, and runtime
libraries into the app rootfs overlay.

```bash
cargo xtask starry app run -t redis --arch riscv64
```

Stress configs are available through explicit QEMU config variants; see
`redis/README.md`.

## GDB Smoke

The `gdb-smoke` case is a QEMU app workflow that prepares a temporary rootfs
overlay with GDB, GDBServer, and tiny debugger smoke targets. Native GDB smoke
and gdbserver smoke are available on x86_64, riscv64, aarch64, and loongarch64.

```bash
cargo xtask starry app qemu -t gdb-smoke --arch x86_64
cargo xtask starry app qemu -t gdb-smoke --arch riscv64
cargo xtask starry app qemu -t gdb-smoke --arch riscv64 \
  --qemu-config qemu-riscv64-gdbserver.toml
cargo xtask starry app qemu -t gdb-smoke --arch riscv64 \
  --qemu-config qemu-riscv64-threads.toml
cargo xtask starry app qemu -t gdb-smoke --arch riscv64 \
  --qemu-config qemu-riscv64-stress.toml
cargo xtask starry app qemu -t gdb-smoke --arch riscv64 \
  --qemu-config qemu-riscv64-gdbserver-manual.toml
```

When using the long-lived Docker container for a `*-manual.toml` entry, run the
same command through `docker exec -it tgoskits-dev ...` so the QEMU serial
console stays interactive.

## MariaDB

The `mariadb` case is a QEMU app workflow that installs MariaDB in the guest,
initializes a fresh data directory, runs an InnoDB SQL workload, and checks that
the data survives a server restart.

```bash
cargo xtask starry app run -t mariadb --arch aarch64
cargo xtask starry app run -t mariadb --arch loongarch64
cargo xtask starry app run -t mariadb --arch x86_64
cargo xtask starry app run -t mariadb --arch riscv64
```

## jcode

The `jcode` case is an x86_64 QEMU app workflow that downloads the jcode AI coding
agent from GitHub releases, patches the glibc-linked binary for musl compatibility
using `patchelf`, builds a glibc stub shared library, and injects everything into
the app rootfs overlay.

```bash
apps/starry/jcode/prepare_jcode_rootfs.sh
cargo xtask starry qemu \
  --arch x86_64 \
  --qemu-config apps/starry/jcode/qemu-x86_64.toml \
  --rootfs tmp/axbuild/rootfs/rootfs-x86_64-jcode.img
```

See `jcode/README.md` for interactive usage and troubleshooting.

## Nginx

The `nginx` case is a QEMU app integration workflow. It installs Alpine nginx
packages in a staging root during prebuild, injects runtime artifacts to the
app overlay, then runs nginx smoke tests inside StarryOS.

```bash
cargo xtask starry app qemu -t nginx --arch x86_64
```

`apps/starry/nginx` keeps the CI-discovered smoke QEMU configs at the app root,
and keeps manual `all`/`phase`/`debug` QEMU configs under `qemu/`. The guest
entrypoint is `runner/nginx-runner.sh`; currently only smoke is connected as the
nginx test entry in tgoskits workflows.

## Orange Pi 5 Plus UVC

The `orangepi-5-plus-uvc` case needs `/usr/bin/uvc-fps` to be installed in the
board rootfs before StarryOS is booted. The usual preparation flow is:

1. reserve the board with `cargo board connect --board-type OrangePi-5-Plus`
   and leave that serial session open;
2. boot into the board Linux shell and read the board IP from the login banner
   or `ip -br addr`;
3. use SSH from the host to copy `apps/starry/orangepi-5-plus-uvc/uvc-fps/`
   into the board Linux system;
4. build and install `uvc-fps` on the board Linux rootfs;
5. close the `cargo board connect` session, then boot StarryOS with:

```bash
cargo xtask starry app board -t orangepi-5-plus-uvc
```

See `orangepi-5-plus-uvc/README.md` for the complete copy, build, install, and
test commands.
