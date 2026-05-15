---
doc_type: CONCERNS
scope: Technical debt, risks, and areas needing attention
---
# Concerns

**Analysis Date:** 2026-05-13

## Technical Debt

### TODO/FIXME Proliferation

The codebase contains ~200+ TODO and ~15 FIXME markers across all three OS systems. Many represent known design gaps rather than implementation polish:

**Highest-impact TODOs:**

- `os/StarryOS/kernel/src/syscall/ipc/msg.rs` (lines 481-861): Blocking send/recv for message queues marked as `"not implemented"`. Waiter wakeup is `"not implemented"`. This means System V msg queues silently drop messages under load.
- `os/axvisor/src/vmm/vcpus.rs:46`: `static mut` vCPU queue with a TODO to replace it. The comment acknowledges `"find a better data structure to replace the static mut"`.
- `os/StarryOS/kernel/src/syscall/fs/memfd.rs:13`: `"TODO: correct memfd implementation"` — the entire memfd subsystem is noted as needing a rewrite.
- `os/axvisor/src/vmm/ivc.rs:247`: Inter-VM communication limited by `"TODO: support larger shared region sizes with alloc_frames API"`.
- `os/StarryOS/kernel/src/syscall/ipc/shm.rs:506-546`: Shared memory page size handling and `SHM_RND`/`SHM_REMAP` flags are unimplemented.
- `os/arceos/modules/axsync/Cargo.toml:30`: FIXME noting the `rand` crate cannot be used due to an upstream `zerocopy` issue.

### Static Mutable State (Deprecated Pattern)

Despite being a Rust codebase, several modules retain `static mut` patterns that Rust has deprecated:

- `os/axvisor/src/hal/mod.rs:74`: `static mut AXVM_PER_CPU: AxVMPerCpu` — per-CPU hypervisor state as `static mut` with `#[allow(static_mut_refs)]`.
- `os/axvisor/src/vmm/vcpus.rs` (lines 58-91): `static mut` vCPU queues with unsafe Send/Sync impl. The vCPU wait queue uses raw pointers through `UnsafeCell` wrappers.
- `os/axvisor/src/vmm/timer.rs` (lines 77-124): `unsafe { TIMER_LIST.current_ref_mut_raw() }` — timer list accessed through unsafe raw mut refs.

### Linter Suppression Debt

`#[allow(...)]` suppressions hide real issues that should be addressed:

- `os/StarryOS/kernel/src/lib.rs:6-7`: `#![allow(missing_docs)]` and `#![allow(clippy::not_unsafe_ptr_arg_deref)]` applied crate-wide, hiding missing documentation and a clippy lint that flags potential unsoundness.
- `os/axvisor/src/hal/mod.rs:107`: `#[allow(static_mut_refs)]` — suppresses the deprecation warning for `static mut`.
- `os/axvisor/src/vmm/fdt/vm_fdt/writer.rs`: 11 `#[allow(dead_code)]` annotations on FDT writer methods.
- `os/StarryOS/kernel/src/pseudofs/dev/tty/terminal/termios.rs:1`: `#![allow(dead_code)]` on the entire termios module.

### Large Files with Low Cohesion

Files exceeding 500 lines often mix concerns:

- `os/StarryOS/kernel/src/pseudofs/usbfs/mod.rs` (1635 lines) — USB filesystem emulation in a single file.
- `os/StarryOS/kernel/src/pseudofs/usbfs/manager.rs` (1365 lines) — USB device manager.
- `os/StarryOS/kernel/src/syscall/ipc/msg.rs` (885 lines) — SysV message queues, with ~50% being TODO stubs.
- `os/StarryOS/kernel/src/pseudofs/proc.rs` (864 lines) — /proc filesystem emulation.
- `os/arceos/modules/axtask/src/run_queue.rs` (743 lines) — task run queue logic.
- `os/StarryOS/kernel/src/syscall/mod.rs` (713 lines) — syscall dispatch table.

### Dead Code Across Systems

Significant dead code marked with `#[allow(dead_code)]` suggests incomplete features and abandoned paths:

- `os/axvisor/src/vmm/fdt/vm_fdt/writer.rs`: 11 dead code annotations for FDT node creation helpers.
- `os/StarryOS/kernel/src/pseudofs/dev/tty/terminal/termios.rs`: Entire module marked dead_code.
- `os/axvisor/src/vmm/vm_list.rs`: Multiple unused VM listing methods.
- `os/StarryOS/kernel/src/file/netlink.rs:63`: `#[allow(dead_code)]` on a Netlink method.
- `os/StarryOS/kernel/src/syscall/net/addr.rs:30`: `#[allow(dead_code)]` on address helpers.

## Risk Areas

### Concurrency and Synchronization (SMP Races)

Recent git history shows active SMP race conditions being discovered and fixed:

- **FD_TABLE synchronization gap** (`git:0fbc13859`, `git:d2e418881`): A TOCTOU window existed between `close_all_fds` and `clone(CLONE_FILES)`. Fixed by adding a synchronization boundary in `FD_TABLE`.
- **FutexGuard::drop TOCTOU** (`git:5e57e8f8a`): A TOCTOU window in futex cleanup was patched. Related SMP concurrent tests were added in `d2e418881`.
- **Futex wait/robust-list hardening** (`git:58d3ec131`): Multiple rounds of futex semantics hardening (PR #545).
- **Test race reproducer rewrite** (`git:b94044431`): The test-futex-race test was rewritten from thread-based to `fork()+MAP_SHARED` because threads sharing memory wasn't producing real races.

**Ongoing risk:** Mixing `spin::Mutex` and `ax_sync::Mutex` across different files (e.g., `os/StarryOS/kernel/src/file/` uses `spin::Mutex` for netlink while `ax_sync::Mutex` for pipes and files). These may have different deadlock-prevention or interrupt-safety properties. No consistent lock ordering is documented.

### Memory Safety Boundaries (Unsafe Blocks)

Unsafe block distribution across the three systems:

| System | Unsafe Occurrences | Primary Location |
|--------|-------------------|------------------|
| StarryOS kernel | ~280 (in kernel/ dir) | `mm/access.rs`, `pseudofs/dev/card*.rs`, `syscall/net/addr.rs` |
| Axvisor | ~200 | `hal/arch/loongarch64/`, `vmm/ivc.rs`, `vmm/vcpus.rs` |
| ArceOS modules | ~519 | `ulib/axlibc/` (FFI wrappers), `axhal/` (platform) |

**Highest-risk unsafe regions:**

- `os/StarryOS/kernel/src/pseudofs/dev/card1.rs` (lines 423-611): DRM ioctl handling transmits user-buffer pointers directly to unsafe pointer casts (`&mut *(data.as_mut_ptr() as *mut DrmGemFlink)`). No validation of the pointee beyond the buffer length.
- `os/StarryOS/kernel/src/mm/access.rs` (lines 305-357): The `VmIo for Vm` trait implementation is `unsafe impl`. It dereferences user-space virtual addresses through raw pointer reads/writes.
- `os/axvisor/src/hal/arch/loongarch64/mod.rs` (lines 29-139): CSR read/write via inline assembly with `#![allow(unsafe_op_in_unsafe_fn)]` — disables the lint that would flag unsafe operations inside unsafe fns.
- `os/StarryOS/kernel/src/mm/io.rs:80`: `vm_read_slice` with unsafe address arithmetic from user-provided iov offsets.
- `os/axvisor/src/vmm/images/linux.rs` (lines 91, 152): `core::ptr::read_unaligned` from raw buffers to parse Linux kernel image headers.

### Unchecked FFI Entry Points

~75 `#[unsafe(no_mangle)]` C-ABI entry points exist across the codebase, concentrated in:

- `os/arceos/ulib/axlibc/src/` — standard C library ABI stubs (io, net, fs, pthread, malloc, etc.)
- `os/arceos/modules/axtask/src/lib.rs:44` — task switch entry point
- `os/arceos/modules/axsync/src/lib.rs:29` — synchronization primitives entry

These functions are called from assembly trampolines or foreign code; invalid arguments could bypass Rust safety checks.

### Platform-Specific Code Divergence

The syscall dispatch table (`os/StarryOS/kernel/src/syscall/mod.rs`) contains 30+ `#[cfg(target_arch = "x86_64")]` directives and 3 `#[cfg(not(..))]` exclusions:

- x86_64 has full coverage for: `mkdir`, `link`, `rmdir`, `unlink`, `symlink`, `rename`, `stat`, `lstat`, `access`, `ftruncate`, `truncate`, `getrlimit`, `setrlimit`, `prlimit64`, `sysinfo` (and many more).
- `riscv64` is excluded from `renameat` support: `#[cfg(not(target_arch = "riscv64"))]`.
- `aarch64` mappings differ from both x86 and riscv (e.g., `fstatat` vs `newfstatat` syscall numbers).
- `loongarch64` has the least coverage — it is not even mentioned in most `#[cfg]` gates.

**Risk:** Applications that depend on x86-only syscalls will silently receive ENOSYS or undefined behavior on other architectures. No compile-time mechanism ensures parity.

### Ephemeral Pseudofs / Device Nodes

The `os/StarryOS/kernel/src/pseudofs/` tree provides Linux-compatible `/proc`, `/dev`, and `/sys` filesystems, but many implementations are placeholder:

- `/proc/self/exe` support is marked FIXME (`mm/loader.rs:293`): `"impl /proc/self/exe to let busybox retry running"`.
- `pseudofs/file.rs:205`: TODO for Linux-like seq file iteration to avoid reading all content at once (large file memory concern).
- `pseudofs/dir.rs:90`: TODO about cacheability of directory ops.
- `/dev/loop` only supports `LOOP_SET_FD`, `LOOP_CLR_FD`, `LOOP_GET_STATUS`, `LOOP_SET_STATUS` — missing `LOOP_GET_STATUS64`, `LOOP_SET_STATUS64`, `LOOP_CONFIGURE`, and other modern loop ioctls.
- `rseq` syscall (`syscall/sync/rseq.rs`): Registration returns `ENOSYS` — libc fast paths that rely on rseq will fall back to slower code paths on all architectures.

## Architectural Gaps

### Syscall Coverage Disparity

The syscall dispatch (`os/StarryOS/kernel/src/syscall/mod.rs`) maps Linux syscalls, but with significant gaps:

| Category | Status |
|----------|--------|
| System V IPC (msg) | Blocking send/recv not implemented; wakeups not implemented |
| System V IPC (shm) | Page size handling incomplete; SHM_RND/SHM_REMAP flags not handled |
| memfd | `"TODO: correct memfd implementation"` |
| rseq | Registration returns ENOSYS (only unregistration works) |
| job control | `syscall/task/job.rs:58`: `"TODO: job control"` |
| open_by_handle_at | Returns `OperationNotSupported` |
| sync/syncfs | `"dummy sys_sync"`, `"dummy sys_syncfs"` — no actual synchronization |
| SIGCONT | `syscall/signal.rs:119`: `"TODO: SIGCONT is allowed to any process in the same session"` |

### Missing /proc and /sys Abstractions

The pseudofs layer (`os/StarryOS/kernel/src/pseudofs/`) provides virtual filesystem emulation, but many Linux semantics are approximated:

- No `/proc/sys/` kernel parameter tree.
- `/proc/cpuinfo`, `/proc/meminfo`, and `/proc/self/maps` are implemented but may return simplified or synthetic data.
- USB filesystem emulation (`pseudofs/usbfs/`) at 3000+ lines is large and complex, suggesting incomplete abstraction.

### Duplicated/Parallel Implementations

Code that exists in multiple forms without clear convergence path:

- **axfs vs axfs-ng**: Two filesystem implementations exist side-by-side (`os/arceos/modules/axfs/` and `os/arceos/modules/axfs-ng/`). axfs-ng appears to be a newer implementation but both are compiled.
- **axnet vs axnet-ng**: Similarly, two network stacks (`os/arceos/modules/axnet/` and `os/arceos/modules/axnet-ng/`). The -ng variants have TODOs about MSS optimization, TCP_INFO, and shutdown handling.
- Dual filesystem backends require maintaining two code paths and two sets of TODOs.

### Test Coverage Gaps

- Only 23 `.rs` test files exist in `test-suit/` for the entire monorepo.
- `scripts/test/clippy_crates.csv` has 103 entries (crates checked by clippy).
- `scripts/test/std_crates.csv` has 53 entries (crates using Rust std).
- Most kernel logic (`os/StarryOS/kernel/src/`) has no unit tests — tests are exclusively integration/system-level via QEMU.
- No coverage tooling is configured for kernel-space code.
- Concurrency test coverage was only recently added (`git:d2e418881`, `git:d9bf2c90b`) for TOCTOU patterns.
- CI uses `fail-fast: true`, so a single flaky test aborts the entire test matrix.

## Known Issues

### Flaky CI Tests (Board Timeouts)

Commit `613c2ce5e` explicitly mentions: `"re-trigger CI (board test flaky timeout)"`. Physical board and QEMU tests suffer from timeout flakiness. CI does not have built-in retry logic for known-flaky tests. The `fail-fast: true` setting in `.github/workflows/ci.yml:159` means a single timeout fails the entire run.

### Compatibility Trade-offs (Loop Device)

The loop device implementation (`os/StarryOS/kernel/src/pseudofs/dev/loop.rs`) uses the `loop_info` struct from `linux_raw_sys` without the 64-bit compat variants (`loop_info64`). The `AnyBitPattern` FIXME comment at line 112 and the TODO at line 116 (`"the following should apply to any block devices"`) indicate incomplete block abstraction. Busybox and systemd utilities that expect `LOOP_GET_STATUS64`/`LOOP_SET_STATUS64` may fail.

### Feature Parity Gaps Between Architectures

x86_64 has the most complete syscall coverage. riscv64 and aarch64 each exclude specific syscalls. loongarch64 has the least coverage. The `renameat` exclusion from riscv64 (`#[cfg(not(target_arch = "riscv64"))]`) may break Rust std's filesystem operations on that platform.

### Zalloc / AnyBitPattern Compatibility

Multiple FIXME comments reference `AnyBitPattern` / `Zeroable` issues (`os/StarryOS/kernel/src/syscall/time.rs:112`, `os/StarryOS/kernel/src/syscall/fs/stat.rs:212`, `os/StarryOS/kernel/src/syscall/resources.rs:33,70`, `os/StarryOS/kernel/src/pseudofs/dev/loop.rs:112`). These relate to `zerocopy` crate version compatibility — some types cannot derive the traits needed for safe zero-initialization. The workaround uses `unsafe { core::mem::zeroed() }` which is UB for types with invalid bit patterns.

## Migration and Upgrade Risks

### Rust Nightly Toolchain Dependency

The project is pinned to `nightly-2026-04-27` (`rust-toolchain.toml`). Any toolchain upgrade requires:

- Testing across 4 architectures (x86_64, aarch64, riscv64, loongarch64).
- Verifying all 62 subtree crates compile with the new nightly.
- Checking for breaking changes in nightly features used: `#![feature(...)]` gates exist throughout ArceOS modules.
- The `unsafe_op_in_unsafe_fn` lint has been deliberately disabled in `os/axvisor/src/hal/arch/loongarch64/` — future nightlies may change this lint's behavior.

### Git Subtree Sync Complexity

With 62 repositories tracked in `scripts/repo/repos.csv`, any change that spans subtree boundaries requires:

- Coordinating PRs across multiple standalone repositories.
- Using `python3 scripts/repo/repo.py pull/push` to sync, which can produce merge conflicts if subtree histories diverge.
- The `--squash` flag in subtree merges loses individual commit history, making bisection difficult.

### Edition 2024 Migration

`rustfmt.toml` uses `style_edition = "2024"` with features like `imports_granularity = "Crate"` and `group_imports = "StdExternalCrate"`. If toolchain support for edition 2024 features changes, formatting could break across the codebase. Edition 2024 is still relatively new and may have corner cases in `no_std`/`no_alloc` environments.

### Cargo Workspace Monolith

All three OS systems (ArceOS, StarryOS, Axvisor), all 62 subtree components, all platform crates, and all drivers share a single Cargo workspace. This means:

- A feature flag change in one crate can trigger recompilation across the workspace.
- No per-system lockstep independence — upgrading a shared dependency affects all three OSes.
- CI must compile the full workspace (or at minimum the dependency resolution) for every change.

## Security Considerations

### Kernel vs Userspace Boundary

The memory access layer (`os/StarryOS/kernel/src/mm/access.rs`) is the primary security boundary. Key concerns:

- The `VmIo for Vm` impl (`access.rs:305`) is `unsafe impl` — a bug in user-memory access validation could allow kernel reads/writes to arbitrary physical memory.
- The `vm_read_uninit().assume_init()` pattern used in syscall handlers (`time.rs:27`, `resources.rs:34`, `loop.rs:113`, `ctl.rs:522,542`) trusts that user-provided memory is valid for the target type. Malformed data could trigger undefined behavior.
- `access_user_memory` closures (`access.rs:312,324`) execute user memory access inside unsafe blocks — if the closure captures state incorrectly, it could leak kernel references.

### System Call Validation Completeness

Syscall dispatch (`os/StarryOS/kernel/src/syscall/mod.rs`) handles ~200+ syscalls through a match statement. The fallthrough (`else =>`) on line 22 logs a warning and returns `ENOSYS` for unknown syscall numbers. However:

- Many valid syscalls return `Ok(0)` without performing any operation (e.g., `sys_sync`, `sys_syncfs`, certain `fcntl` options).
- Signo validation for signal syscalls checks bounds, but `SIGCONT` job control semantics are explicitly TODO.
- `prctl` syscall (`syscall/task/ctl.rs:146`) logs `"unsupported option"` and returns `EINVAL` for many Linux prctl options — processes that rely on these (e.g., `PR_SET_SECCOMP`) will fail.

### Resource Cleanup in Error Paths

- `os/axvisor/src/shell/command/vm.rs:761`: `"TODO: Clean up VM-related data files"` — VM deletion may leave stale files.
- `os/StarryOS/kernel/src/entry.rs:74`: `"TODO: wait for all processes to finish"` — process group cleanup on shutdown is incomplete.
- No explicit OOM handling beyond page fault rejection — if memory allocation fails in kernel paths, the behavior is undefined rather than graceful degradation.
- `os/StarryOS/kernel/src/syscall/fs/ctl.rs:641-646`: `sync` and `syncfs` are dummies — data integrity on crash is not guaranteed.

### Signal Handling Robustness

- `os/StarryOS/kernel/src/syscall/signal.rs:119`: SIGCONT delivery ignores POSIX session/job control requirements.
- `os/StarryOS/kernel/src/syscall/task/wait.rs:74`: `"FIXME: add back support for WALL & WCLONE, since ProcessData may drop before"` — waitpid flags are incomplete, which could cause zombie process leaks.
- `os/StarryOS/kernel/src/syscall/task/execve.rs:56`: `"TODO: handle multi-thread case"` — execve in a multi-threaded process (which should kill all other threads) is incomplete.

## Performance Bottlenecks

### Global Lock Contention

- `os/StarryOS/kernel/src/file/mod.rs:216`: `FD_TABLE` is a single `Arc<RwLock<FlattenObjects<...>>>` for the entire process. Under high thread counts performing file operations, this becomes a contention point.
- `os/StarryOS/kernel/src/file/fs.rs:21`: `FS_CONTEXT.lock()` is held for filesystem namespace operations.
- `os/StarryOS/kernel/src/mm/aspace/mod.rs`: The address space (`Arc<Mutex<AddrSpace>>`) is locked during all memory operations including page fault handling — a page fault in one thread blocks all other memory operations.

### Known Inefficiencies

- `os/arceos/modules/axfs/src/fs/fatfs.rs:198-262`: FAT filesystem operations call `file.seek()` before every read/write — `"TODO: more efficient"` is noted five times.
- `os/StarryOS/kernel/src/mm/access.rs:93`: Multi-page reads loop through `vm_read_one_page` sequentially — `"TODO: this is inefficient, but we have to do this instead of"` (truncated).
- `os/arceos/modules/axnet-ng/src/device/ethernet.rs:365`: ARP resolution logic described as `"TODO: optimize logic such that one long-pending ARP"` blocks subsequent lookups.
- `os/axvisor/src/vmm/vcpus.rs:485`: Interrupt dispatch noted as `"TODO: maybe move this irq dispatcher to lower layer to accelerate the interrupt handling"`.

## Dependencies at Risk

- **`rand` / `zerocopy` incompatibility** (`os/arceos/modules/axsync/Cargo.toml:30`): The `rand` crate cannot be used until `zerocopy` upstream PR #2574 is resolved. This blocks any feature that requires random number generation in synchronization primitives.
- **`linux_raw_sys` version lock**: Multiple FIXME comments related to `AnyBitPattern`/`Zeroable` suggest the `zerocopy` dependency version is pinned and cannot be updated.
- **Rust nightly channel**: Any upstream change to nightly features used (e.g., `asm_const`, `asm_experimental_arch`, `naked_functions`, or `strict_provenance`) could break the build.
- **smoltcp fork**: ArceOS uses a vendored/forked smoltcp — upstream changes to smoltcp must be manually integrated.

## Fragile Areas

### DRM/GPU Device Emulation

`os/StarryOS/kernel/src/pseudofs/dev/card0.rs` and `card1.rs` implement DRM ioctl handling with extensive unsafe pointer casts. These are the most unsafe-dense files in the kernel and deal with complex Linux DRM ABI structures. A type layout mismatch between the kernel and userspace struct definitions would cause silent memory corruption.

### FDT (Flattened Device Tree) Handling

`os/axvisor/src/vmm/fdt/` contains the FDT parser, printer, and writer (6 files). The parser reads from raw device tree blobs using `unsafe { core::slice::from_raw_parts(fdt_vaddr.as_ptr(), ...) }`. A malformed or adversarial DTB from a VM image could cause out-of-bounds reads. The TODO at `parser.rs:681` about filtering by compatible property suggests incomplete validation of the device tree structure.

---

*Concerns audit: 2026-05-13*
