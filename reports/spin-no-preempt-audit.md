# `SpinNoPreempt` usage audit

Date: 2026-05-22

This note records the current `SpinNoPreempt` and direct `NoPreempt` usage
sites. It is a follow-up to the external `spin` migration notes and focuses on
the risks introduced by disabling preemption without disabling local IRQs.

## Rule of thumb

`ax_kspin::SpinNoPreempt<T>` is an atomic-context spin lock:

- locking disables kernel preemption;
- locking does not disable local IRQs;
- lockdep tracks it when `ax-kspin/lockdep` is enabled.

The API documentation in `components/kspin/src/lib.rs` says it must either be
used while local IRQs are already disabled, or never be used from interrupt
handlers.

That creates two independent hazards:

1. Same-CPU IRQ reentry deadlock. If a task holds a `SpinNoPreempt` lock with
   local IRQs enabled and an IRQ handler or IRQ-triggered waker tries to acquire
   the same lock on the same CPU, the handler spins forever because the lock
   holder cannot resume.
2. Atomic-context sleep violation. While the guard is alive,
   `axtask::might_sleep()` sees `preempt_count != 0`. User-memory page faults,
   blocking mutexes, scheduler paths, and filesystem or device callbacks that
   can sleep must not run under the guard.

`SpinNoIrq` only addresses the first hazard. It still creates atomic context and
is not a repair for code that may sleep, reschedule, fault on user memory, or
call a backend that may do so.

## Search commands

```text
rg -n "SpinNoPreempt|SpinNoPreemptGuard|BaseSpinLock<NoPreempt|NoPreempt" \
  --glob '*.rs' --glob '!target/**'

rg -n "ax_kernel_guard::NoPreempt|NoPreempt::new\\(|NoPreemptGuard::new\\(" \
  --glob '*.rs' --glob '!target/**'
```

## Summary

Lower-risk, short critical-section users:

- `os/StarryOS/kernel/src/pseudofs/dev/loop.rs`

Intentional direct `NoPreempt` users:

- `os/arceos/modules/axhal/src/irq.rs`
- `components/percpu/percpu/src/custom/mod.rs`
- `components/percpu/percpu_macros/src/lib.rs`

## Filesystem locks in `axfs-ng`

### FAT

`os/arceos/modules/axfs-ng/src/fs/fat/fs.rs` used to alias:

```rust
use ax_kspin::{SpinNoPreempt as Mutex, SpinNoPreemptGuard as MutexGuard};
```

The main filesystem state is now protected by `ax_sync::Mutex`, a
task-context blocking mutex when multitasking is enabled.

Risk:

- `FatFileNode::{read_at,write_at,append,set_len,sync}` hold the lock while
  calling `fatfs` methods.
- Those methods call `SeekableDisk::{read,write,flush}`, which call block-device
  `read_block`, `write_block`, or `flush`.
- Directory operations hold the same lock across `fatfs` iteration and metadata
  mutation.
- `read_dir` also calls the external `DirEntrySink::accept` callback while the
  filesystem lock is held. The VFS trait already warns that sinks should not
  operate on nodes because some filesystems hold a lock while iterating.

Assessment: resolved for the main FAT state lock by moving it to
`ax_sync::Mutex`. This preserves filesystem-wide serialization without running
block I/O in `SpinNoPreempt`/`SpinNoIrq` atomic context. The lock is still large,
so future work may split it or avoid calling `DirEntrySink::accept` while it is
held, but it is no longer part of the `SpinNoPreempt` hazard.

`root_dir: Mutex<Option<DirEntry>>` uses the same blocking mutex alias after the
change. It is used only to install and clone the root dentry and can later
become `OnceCell` if desired.

### ext4 with `rsext4`

`os/arceos/modules/axfs-ng/src/fs/ext4/rsext4/fs.rs` used to alias
`SpinNoPreempt` as the filesystem mutex protecting `Ext4State`. It now uses
`ax_sync::Mutex`.

Risk:

- `sync_to_disk()` holds the lock while flushing all block, bitmap, and inode
  caches, syncing the superblock and group descriptors, committing the journal,
  and calling `dev.cantflush()`.
- `read_at()` holds the lock while resolving extents and loading data blocks.
- `write_at()`, `append()`, `set_len()`, `set_symlink()`, `create()`, `link()`,
  `unlink()`, and `rename()` hold the lock across rsext4 operations that may
  allocate, touch caches, and call the block device.
- The loop-device adapter keeps `flush()` as a no-op to avoid re-entering
  backing-file VFS writeback from filesystem block I/O paths.

Assessment: resolved for the filesystem mutex by moving it to
`ax_sync::Mutex`. This is still a coarse filesystem lock, but disk, cache, and
journal operations are no longer wrapped by a `SpinNoPreempt` guard.

### ext4 with `lwext4`

`os/arceos/modules/axfs-ng/src/fs/ext4/lwext4/fs.rs` used the same
`SpinNoPreempt` alias for `LwExt4Filesystem`. It now uses `ax_sync::Mutex`.

Risk:

- `read_at`, `write_at`, `append`, `set_len`, `set_symlink`, `read_dir`,
  `lookup`, `create`, `link`, `unlink`, and `rename` call into `lwext4_rust`
  while holding the lock.
- The `flush()` implementation directly calls `self.inner.lock().flush()`.

Assessment: resolved for the filesystem mutex by moving it to
`ax_sync::Mutex`. It remains a coarse lock around the lwext4 wrapper and can be
split later if contention or callback constraints require it.

## Starry epoll

`os/StarryOS/kernel/src/file/epoll.rs` used to use `SpinNoPreempt` for:

- `EpollInterest::mode`;
- `EpollInner::interests`;
- `EpollInner::ready_queue`.

All three now use `SpinNoIrq`. `ready_queue` needed the change because it can
be touched by `InterestWaker::wake_by_ref()`, and wakers may be invoked from IRQ
wake paths. `mode` and `interests` are short critical sections without a proven
outer IRQ-disabled context, so they also follow the conservative rule that
`SpinNoPreempt` should not be used there.

Risk:

- `mode` is a short state lock. It is currently taken in task-side epoll paths
  and does not wrap user-memory access or blocking operations.
- `interests` is taken in `add`, `modify`, `delete`, and stale-entry removal.
  It wraps `HashMap` mutation and some `Arc` replacement/drop work, but does not
  call `FileLike::poll` or `register` while held.
- `ready_queue` is different from `mode` and `interests`:
  `InterestWaker::wake_by_ref()` pushes into it. That waker can be invoked by a
  `PollSet::wake()` path. Some poll sets are woken from IRQ handlers, for
  example the Starry UART IRQ path calls `poll.wake()` after filling its RX
  buffer.

Assessment: medium residual risk. Moving the epoll locks to `SpinNoIrq` closes
the immediate same-CPU IRQ reentry hole. It does not solve the fact that
`VecDeque::push_back` may allocate from a waker path. A follow-up should make
epoll wake enqueueing IRQ-safe explicitly by preallocating/bounding the queue or
deferring heap-growing work out of IRQ context. `interests` also uses a
`HashMap`, but that path is driven by `epoll_ctl` style task-context operations,
not by the waker fast path.

## Starry loop-device cache

`os/StarryOS/kernel/src/pseudofs/dev/loop.rs` uses
`SpinNoPreempt<Vec<Vec<u8>>>` for `CacheData::blocks`.

Risk:

- `LoopBlockDevice::{read_block,write_block}` hold the lock only while copying
  data between the cache and caller buffers and updating the dirty flag.
- `writeback_buffer()` copies one cache chunk into a stack buffer while holding
  the lock, then drops the lock before calling `FileBackend::write_at` or
  `sync`.
- The comments explicitly avoid doing VFS writeback from filesystem block I/O
  paths.

Assessment: lower risk than the filesystem locks. The critical sections are
bounded memory copies and do not intentionally sleep. It is still coupled to
filesystem block I/O serialization, so do not change it to a sleepable mutex
without checking all block-device caller contexts.

## Starry tty metadata

`os/StarryOS/kernel/src/pseudofs/dev/tty/terminal/mod.rs` used
`SpinNoPreempt` for:

- `window_size`;
- `termios`.

Both now use `SpinNoIrq`, because these short locks were taken without a proven
outer IRQ-disabled context.

Risk:

- ioctl paths copy user data before acquiring the lock on write-side updates.
  Existing comments call out that user-memory access under the guard would page
  fault and panic in `might_sleep()`.
- read-side paths clone/copy the data under the lock and then perform later
  work after the guard is dropped.
- The lock is not currently taken from the serial IRQ handler; IRQ paths wake a
  `PollSet` and do not touch terminal termios/window-size state.

Assessment: low risk if the current pattern is preserved. Keep the rule that
all `vm_read`, `vm_write`, blocking operations, and line-discipline work happen
outside the guard. A small helper that loads/stores termios/window-size by value
would reduce the chance of future call sites accidentally extending the guard
lifetime.

## Starry pty producer

`os/StarryOS/kernel/src/pseudofs/dev/tty/pty.rs` used
`Arc<SpinNoPreempt<Prod<Buffer>>>` in `PtyWriter`. It now uses `SpinNoIrq`,
because the producer lock is not taken under a proven outer IRQ-disabled
context.

Risk:

- `write()` holds the lock only for `push_slice(buf)`.
- `PollSet::wake()` is called after the guard is dropped.
- The copied amount is bounded by the 4 KiB PTY buffer.

Assessment: low risk. The current lock does not wrap a wakeup, user-memory
access, or blocking operation. If future writers can run directly in IRQ
context, change the lock strategy; with the current task-side writer model this
can remain a short spin critical section.

## Direct `NoPreempt` users

`os/arceos/modules/axhal/src/irq.rs` creates a `NoPreempt` guard in
`handle_irq()`. This is intentional: the function already runs in interrupt
context, so local IRQs are expected to be disabled by the trap path, and the
guard prevents scheduler preemption until the handler returns.

`components/percpu/percpu/src/custom/mod.rs` and generated percpu macro code use
`NoPreempt` only around current-CPU percpu access. That prevents migration while
accessing CPU-local storage and does not protect shared data with a spin lock.
It is outside the main `SpinNoPreempt` lock audit, but the same rule applies:
do not add sleeping work inside those guarded closures.

## Recommended order

1. Review the residual `epoll.ready_queue` allocation path after the IRQ-safe
   lock changes.
2. Reevaluate the loop-device cache lock only after checking every block-device
   caller context; keep it as a bounded memory-copy spin critical section for
   now.
3. Keep tty termios/window-size and pty producer locks as short `SpinNoIrq`
   critical sections, but add helper APIs or comments if future edits start
   extending guard lifetimes.
4. Leave `axhal` IRQ and percpu `NoPreempt` guards as intentional uses unless a
   specific sleeping path is introduced under them.
