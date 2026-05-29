# Starry PostgreSQL App

This case runs a PostgreSQL smoke test in StarryOS through the app runner.

```bash
cargo xtask starry app run -t postgresql --arch x86_64
cargo xtask starry app run -t postgresql --arch aarch64
cargo xtask starry app run -t postgresql --arch riscv64
cargo xtask starry app run -t postgresql --arch loongarch64
```

The guest test uses prebuilt PostgreSQL 16.4 binaries injected by `prebuild.sh`.
It initializes a fresh database cluster via `initdb`, starts `postgres` over TCP
on port 5433, verifies `SELECT 1`, then runs a structured SQL workload over the
`starry_test` database. The workload covers:

- DDL: table creation with foreign keys and indexes
- DML: multi-row inserts, updates, deletes
- Queries: filtering, ordering, aggregation, joins
- Transactions: commit and rollback
- Bulk insert via `generate_series`
- Persistence: server restart and data verification

Before injecting the test script, `prebuild.sh` refreshes the app-specific
rootfs from the cached clean Alpine archive, cross-compiles PostgreSQL 16.4
with musl for the target architecture, and copies the binaries and their
runtime library dependencies into the rootfs overlay.

## Prerequisites

The `prebuild.sh` script requires a musl cross-compiler for the target
architecture:

| Architecture | Cross-compiler package |
|-------------|----------------------|
| riscv64 | `riscv64-linux-musl-gcc` |
| aarch64 | `aarch64-linux-musl-gcc` |
| x86_64 | `x86_64-linux-musl-gcc` |
| loongarch64 | `loongarch64-linux-musl-gcc` |

On macOS, these can be installed via [musl-cross-make](https://github.com/richfelker/musl-cross-make).
On Linux, use your distribution's cross-compiler packages.

## Required Kernel Patches

Running PostgreSQL requires the following kernel patches (all merged to tgoskits `dev`):

- Process credentials subsystem (setuid/setresuid/getuid etc.)
- SA_RESTART syscall restart for signal-interrupted syscalls
- DTB-based physical memory discovery (PostgreSQL needs ~300MB+)
- RLIMIT_STACK default set to 8MB (Linux default)
- fsync/fdatasync directory support
- sync_file_range stub
- prctl PR_SET_PDEATHSIG support
- epoll_pwait sigsetsize compatibility (musl's 16-byte sigset_t)

## Known Limitations

- `initdb` is slow (~40s on riscv64 QEMU) due to emulation overhead
- Requires dynamic linking with `--export-dynamic` for extension `dlopen`
- `pgbench` TPC-B runs pending per-file uid/gid storage in VFS
