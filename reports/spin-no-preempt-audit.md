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

The main filesystem state is now protected by `SpinNoIrq`.

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

Assessment: the original `SpinNoPreempt` same-CPU IRQ reentry hazard is resolved
by moving the lock to `SpinNoIrq`. A previous attempt to use `ax_sync::Mutex`
exposed that Starry rootfs setup still takes the filesystem lock in an early
atomic context (`irq_enabled=false`, `preempt_count=1`), where a blocking mutex
panics in `might_sleep()`. The lock is still large and may wrap block I/O, so
future work should split it or move rootfs mount/flush paths into a normal task
context.

`root_dir: Mutex<Option<DirEntry>>` uses the same `SpinNoIrq` alias after the
change. It is used only to install and clone the root dentry and can later
become `OnceCell` if desired.

### ext4 with `rsext4`

`os/arceos/modules/axfs-ng/src/fs/ext4/rsext4/fs.rs` used to alias
`SpinNoPreempt` as the filesystem mutex protecting `Ext4State`. It now uses
`SpinNoIrq`.

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

Assessment: the `SpinNoPreempt` same-CPU IRQ reentry hazard is resolved by
moving the filesystem mutex to `SpinNoIrq`. This is a conservative compatibility
fix for current Starry boot/rootfs paths; it does not make the coarse lock safe
for sleeping work. Disk, cache, and journal operations can still run in atomic
context while the guard is held, so the longer-term fix is to avoid invoking
these filesystem paths before the kernel has a sleepable task context, or to
split the filesystem serialization strategy.

### ext4 with `lwext4`

`os/arceos/modules/axfs-ng/src/fs/ext4/lwext4/fs.rs` used the same
`SpinNoPreempt` alias for `LwExt4Filesystem`. It now uses `SpinNoIrq`.

Risk:

- `read_at`, `write_at`, `append`, `set_len`, `set_symlink`, `read_dir`,
  `lookup`, `create`, `link`, `unlink`, and `rename` call into `lwext4_rust`
  while holding the lock.
- The `flush()` implementation directly calls `self.inner.lock().flush()`.

Assessment: the `SpinNoPreempt` same-CPU IRQ reentry hazard is resolved by
moving the filesystem mutex to `SpinNoIrq`. As with rsext4, this keeps current
early/rootfs contexts working but leaves the broader coarse-lock and block-I/O
under atomic context issue for a later design change.

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

`os/StarryOS/kernel/src/pseudofs/dev/loop.rs` used
`SpinNoPreempt<Vec<Vec<u8>>>` for `CacheData::blocks`. It now uses
`SpinNoIrq`, because ext4 can call the loop block-device adapter while holding
filesystem locks that already put the kernel in atomic context. A blocking
mutex panicked in the `util-linux` Starry QEMU case when `read_block()` tried to
lock the cache under the ext4 filesystem guard.

Risk:

- `LoopBlockDevice::{read_block,write_block}` hold the lock only while copying
  data between the cache and caller buffers and updating the dirty flag.
- `writeback_buffer()` copies one cache chunk into a stack buffer while holding
  the lock, then drops the lock before calling `FileBackend::write_at` or
  `sync`.
- The comments explicitly avoid doing VFS writeback from filesystem block I/O
  paths.

Assessment: the `SpinNoPreempt` same-CPU IRQ reentry hazard is resolved by
using `SpinNoIrq`, and the observed blocking-mutex panic is avoided without
re-entering VFS writeback from ext4 block-I/O callbacks. The critical sections
remain bounded memory copies. If this cache ever needs to allocate or perform
VFS I/O under the guard, the strategy must be revisited.

## Starry tmpfs and VFS cache nesting

`os/StarryOS/kernel/src/pseudofs/tmp.rs` now uses `SpinNoIrq` for the tmpfs
root dentry and per-directory entry maps.

Risk:

- `MemoryFs::root_dir()` is called while mounting pseudofs during startup.
  Using a blocking mutex there caused `might_sleep()` to panic when the startup
  path reached tmpfs before a sleepable context was available.
- `components/axfs-ng-vfs/src/node/dir.rs` protects its dentry cache with
  `SpinNoIrq` and calls filesystem `lookup`, `create`, `unlink`, and
  `open_file` paths while the cache guard is held. A blocking tmpfs directory
  map mutex therefore panicked when a tmpfs cache miss reached
  `MemoryNode::lookup()`.
- `MemoryNode::link()` no longer calls the generic `DirEntry::metadata()` while
  holding the tmpfs entries guard; it reads the target tmpfs inode metadata
  directly from the short `SpinNoIrq` metadata lock.

Assessment: the observed Starry QEMU panics are resolved by keeping tmpfs root
and directory maps non-blocking. This remains a compatibility fix for the
current VFS cache contract. A broader VFS improvement would avoid calling
filesystem operations while holding the dentry-cache spin guard, which would
let tmpfs directory maps move back to a sleepable lock if desired.

## Unix domain socket path binding

`os/arceos/modules/axnet-ng/src/unix/mod.rs` no longer executes transport
callbacks while holding `DirEntry::user_data()`'s `SpinNoIrq` guard.

Risk:

- `/dev/log` setup creates a Unix datagram socket and binds it to a filesystem
  path during Starry pseudofs initialization.
- Path-based Unix sockets store their `BindSlot` in VFS `user_data`, whose lock
  is a `SpinNoIrq` guard from `axfs-ng-vfs`.
- The old code passed a borrowed `BindSlot` directly into the callback while
  the `user_data` guard was still alive. `DgramTransport::bind()` then tried to
  take its sleepable socket mutex under that spin guard and tripped
  `might_sleep()`.

Assessment: resolved by cloning the `Arc<BindSlot>` out of `user_data`, dropping
the spin guard, and only then invoking the transport operation. This keeps
socket internals sleepable while preserving the short VFS user-data critical
section.

## Starry packet and netlink receive paths

`os/StarryOS/kernel/src/file/netlink.rs` and
`os/StarryOS/kernel/src/file/packet.rs` hit the second `SpinNoIrq` hazard:
faultable user-memory writes while a spin guard was alive.

Risk:

- `NetlinkSocket::read_one()` used to pop and copy the queued netlink message
  while holding the socket queue guard. `c-regression/test-netlink-genl`
  faulted during `IoDst::write()` and tripped the
  "faultable user memory access requires IRQs enabled" assertion.
- `PacketSocket::recv_packet()` used to write the pending packet into the user
  buffer inside the socket state critical section. `bugfix/bug-packet-arping`
  reproduced the same user-copy assertion.

Assessment: resolved by moving the queued packet/message into a local variable
under the guard, dropping the guard, and only then copying into user memory.
The locks still protect only queue/state mutation; page-faultable user access
now happens with IRQs enabled.

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
2. Keep tty termios/window-size and pty producer locks as short `SpinNoIrq`
   critical sections, but add helper APIs or comments if future edits start
   extending guard lifetimes.
3. Leave `axhal` IRQ and percpu `NoPreempt` guards as intentional uses unless a
   specific sleeping path is introduced under them.
