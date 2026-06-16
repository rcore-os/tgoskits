# Starry Redis App

This case runs Redis inside StarryOS through the app runner.

Default QEMU configs run the functional Redis app checks:

```bash
cargo xtask starry app qemu -t redis --arch riscv64
```


The Redis AOF appendonly regression is kept as an explicit app config:

```bash
cargo xtask starry app qemu \
  -t redis \
  --arch riscv64 \
  --qemu-config qemu-riscv64-aof-appendonly.toml
```

Stress configs are available as explicit QEMU config variants:

```bash
cargo xtask starry app qemu \
  -t redis \
  --arch riscv64 \
  --qemu-config qemu-riscv64-stress.toml
```

The prebuild step installs Redis into a temporary Alpine staging root and injects
only the Redis binaries, test scripts, and needed runtime libraries into the app
rootfs overlay.

Legacy C reproduction assets live under `regressions/`; they are kept with the
Redis app because they require Redis binaries and are not part of the Starry
system test-suit grouped runner.
