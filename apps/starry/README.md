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

The `gdb-smoke` case is a RISC-V QEMU app workflow that prepares a temporary
rootfs overlay with GDB, GDBServer, and two tiny target programs.

```bash
cargo xtask starry app run -t gdb-smoke --arch riscv64
cargo xtask starry app run -t gdb-smoke --arch riscv64 \
  --qemu-config qemu-riscv64-gdbserver.toml
```

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
