# Debug Mode Rules — StarryOS

- `make debug` runs QEMU with GDB attached; `make justrun` runs without rebuilding (faster iteration)
- CI boot test (`make ARCH=<arch> ci-test`) uses `scripts/ci-test.py` connecting via TCP port 4444, waiting for "starry:~#" BusyBox shell prompt
- `DummyFd` returns different values under QEMU vs real hardware — dummy FDs on real HW, `Unsupported` under QEMU. Check `kernel/src/syscall/fs/io.rs` if syscall behavior differs between environments
- Disk image is NOT reset between runs — switching architectures without `make rootfs` causes stale/corrupt filesystem bugs
- Build dependencies (cargo-axplat, ax-config-gen, cargo-binutils) are auto-installed at build time — if install fails, check network/proxy, not local setup
- `SKIP_QEMU=true` env var skips QEMU boot tests in `scripts/test.sh`
- Unit tests (`make unittest`) unset `AX_CONFIG_PATH` to load a dummy config — test failures may indicate config-dependent code paths
- `DWARF=y` is default (debug info enabled); disable with `DWARF=n` if GDB symbols are misleading
- `make rv` / `make la` are convenience aliases for riscv64 / loongarch64
