# Zephyr periodic latency sampler

This application runs a 10 ms absolute-deadline task for 300 samples and
prints CSV rows that can be summarized by `scripts/test/rt_latency_stats.py`.

## Build

The Day4 baseline used Zephyr commit `aa37fa1ebc92` and the existing AArch64
musl cross compiler:

```bash
export ZEPHYR_BASE=/tmp/zephyrproject/zephyr
export ZEPHYR_TOOLCHAIN_VARIANT=cross-compile
export CROSS_COMPILE="$HOME/.local/toolchains/aarch64-linux-musl-cross/bin/aarch64-linux-musl-"

west build -p always -b qemu_cortex_a53 \
  scripts/test/zephyr-periodic \
  -d /tmp/zephyr-periodic-build
```

GCC 11 does not define the `ID_AA64ISAR2_EL1` register alias used by this
Zephyr revision. The recorded build replaced that alias in
`include/zephyr/arch/arm64/lib_helpers.h` with its architectural encoding
`S3_0_C0_C6_2`. A newer compatible AArch64 compiler does not need this local
source-tree workaround.

## Run

Run the native QEMU reference with:

```bash
west build -d /tmp/zephyr-periodic-build -t run
```

AxVisor's memory image loader copies raw bytes and does not parse ELF files.
Use `zephyr.bin`, never `zephyr.elf`:

```bash
cp /tmp/zephyr-periodic-build/zephyr/zephyr.bin \
  tmp/axbuild/rootfs/qemu-aarch64/zephyr/zephyr-periodic

readelf -h /tmp/zephyr-periodic-build/zephyr/zephyr.elf | grep 'Entry point'
```

Copy `axvisor-qemu-aarch64.toml` to a local VM config, replace `kernel_path`,
and update `entry_point` if the ELF entry differs from `0x40001044`. Then run:

```bash
cargo xtask axvisor qemu \
  --config os/axvisor/configs/board/qemu-aarch64.toml \
  --qemu-config os/axvisor/configs/qemu/qemu-aarch64.toml \
  --vmconfigs /path/to/zephyr-periodic.toml \
  --rootfs /path/to/rootfs-aarch64-alpine.img
```

Completion is proven only by both the CSV header and:

```text
PERIODIC LATENCY COMPLETE samples=300
```
