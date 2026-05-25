# Starry Redis App

This case runs Redis inside StarryOS through the app runner.

Default QEMU configs run the functional Redis app checks:

```bash
cargo xtask starry app run -t redis --arch riscv64
```

Stress configs are available as explicit QEMU config variants:

```bash
cargo xtask starry app run \
  -t redis \
  --arch riscv64 \
  --qemu-config qemu-riscv64-stress.toml
```

The prebuild step installs Redis into a temporary Alpine staging root and injects
only the Redis binaries, test scripts, and needed runtime libraries into the app
rootfs overlay.
