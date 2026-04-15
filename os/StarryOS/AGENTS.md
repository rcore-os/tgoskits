# AGENTS.md

This file provides guidance to agents when working with code in this repository.

- Build: `make build` (default), `make run` (build+QEMU), `make justrun` (skip rebuild), `make debug` (GDB attached)
- `arceos/` is a git submodule EXCLUDED from the workspace — ax-* crates come from crates.io (v0.5.0), not path deps
- Dual error types: `AxResult/AxError` (internal kernel), `LinuxResult/LinuxError` (syscall returns, negative errno), `VmResult/VmError` (VM ops). Chain: AxError → LinuxError → negative errno
- Disk file persists across runs — switching architectures requires `make rootfs` or stale filesystem bugs
- `NO_AXSTD=y` changes feature prefix from `axstd/` to `axfeat/`; `DWARF=y` (default) adds debug info; `LTO=y` enables LTO+codegen-units=1
- `make ARCH=<arch> ci-test` uses Python script on TCP port 4444 waiting for "starry:~#" prompt
- Unit tests (`make unittest`) unset `AX_CONFIG_PATH` to load dummy config
- Build deps (cargo-axplat, ax-config-gen, cargo-binutils) are AUTO-INSTALLED — never manually install
- `#[unsafe(no_mangle)]` required on entry points (Rust edition 2024), not just `#[no_mangle]`
- `DummyFd` pattern: unimplemented syscalls return dummy FDs EXCEPT under QEMU where they return Unsupported
- Conventional Commits required: `type(scope): subject`
