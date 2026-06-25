# StarryOS tmpfs lockdep analysis

## Background

Running:

```text
FEATURES=lockdep cargo xtask starry test qemu --arch riscv64
```

triggered:

```text
lockdep: lock order inversion detected
requested:
  kind=mutex id=98 class=23 addr=0xffffffc0807ce218 acquire_at=os/StarryOS/kernel/src/pseudofs/tmp.rs:174:43
conflicting held lock:
  id=94 class=23 addr=0xffffffc0807ce418 acquired_at=os/StarryOS/kernel/src/pseudofs/tmp.rs:388:39
held stack:
  [0] held: id=23 class=11 addr=0xffffffc080949e10 acquired_at=os/StarryOS/kernel/src/pseudofs/mod.rs:64:25
  [1] top: id=94 class=23 addr=0xffffffc0807ce418 acquired_at=os/StarryOS/kernel/src/pseudofs/tmp.rs:388:39
```

The same lockdep report is also reproducible with a targeted Starry test case:

```text
TMPDIR=/tmp FEATURES=lockdep cargo xtask starry test qemu --arch riscv64 -g normal -c test-shm-deadlock
```

This note records the current analysis before changing `tmp.rs`.

## Relevant code path

The reported lock sites match the following flow in `os/StarryOS/kernel/src/pseudofs/tmp.rs`:

1. `MemoryNode::create()` locks the parent directory entries at `tmp.rs:388`.
2. While the parent `entries` mutex is still held, it calls `Inode::new()` at `tmp.rs:393`.
3. If the new inode is a directory, `Inode::new()` locks the new directory's `entries` mutex at `tmp.rs:174` in order to insert `"."` and `".."`.

So the immediate runtime nesting is:

```text
parent_dir.entries.lock() -> child_dir.entries.lock()
```

The extra held lock shown from `os/StarryOS/kernel/src/pseudofs/mod.rs:64` is `FS_CONTEXT.lock()` held by `mount_all()`. It is not the direct cause of the directory lock report.

## Relation to `test-shm-deadlock`

The targeted `test-shm-deadlock` reproduction does not change the reported lock sites:

- requested lock: `os/StarryOS/kernel/src/pseudofs/tmp.rs:174`
- conflicting held lock: `os/StarryOS/kernel/src/pseudofs/tmp.rs:388`
- extra held lock: `os/StarryOS/kernel/src/pseudofs/mod.rs:64`

So the reproduced failure still lands in pseudofs tmpfs initialization, not in the SysV shared-memory lock ordering that `test-shm-deadlock` is intended to exercise.

That test case itself targets the `SHM_MANAGER -> ShmInner` ordering in `os/StarryOS/kernel/src/syscall/ipc/shm.rs`, but the observed panic happens earlier on the tmpfs path used during pseudofs setup, including mounts such as `/dev/shm`, `/tmp`, `/sys`, and the follow-up directory creation below `/sys/class/...`.

Current implication:

- the test command is a valid reproducer for the tmpfs lockdep report;
- it is not, by itself, evidence of a new regression in the SHM lock ordering code path.

## Why this is not a blanket same-class rule

`lockdep` does not reject nested locks just because they have the same class. The order-inversion check in `components/lockdep/src/state.rs` only fails when the dependency graph already contains a reverse edge:

```text
requested_class -> held_class
```

Specifically:

- `check_can_acquire()` reports `OrderInversion` only if `graph.reaches(class_id, held.class_id)` is true.
- `record_edges()` adds edges from every currently held class to the newly acquired class.

That means the report is not caused by a hardcoded "same class may never nest" rule.

## Why this specific report is likely conservative

The two directory-entry mutexes are different lock instances (`id=94` and `id=98`), but they ended up with the same `class=23`.

That can happen because runtime-created `ax_sync::Mutex` values use `RawMutex::INIT`, which contains `LockdepMap::new_dynamic()` in `os/arceos/modules/axsync/src/mutex.rs`. For a dynamic lock map, the class key is filled lazily in `components/lockdep/src/state.rs:465-500` from the tracked caller of the first successful lock preparation.

For `tmpfs` directory entry mutexes, many instances can therefore collapse into the same logical class when they are first observed at the same `entries.lock()` call site.

Under that model, the reported stack is best read as:

```text
directory entries class -> directory entries class
```

rather than as proof that these two specific inode instances have already been seen in the reverse order.

For the `create()` path alone, this makes the report look like a conservative false positive:

- the code is taking a parent directory lock and then a freshly created child directory lock;
- the two locks are different instances;
- the class granularity is too coarse to express "same kind of lock, but different directory objects in a tree relationship".

Another reason this looks conservative is that the requested lock at `tmp.rs:174` is taken only while initializing the brand-new child directory's `"."` and `".."` entries. In that local path:

- the child inode has just been allocated;
- the child directory is not yet published into the parent map until `tmp.rs:394`;
- the nesting is structurally parent -> newly created child, not an arbitrary peer -> peer relation.

That makes this specific stack a poor match for a real two-way deadlock cycle, even though the class-level graph cannot distinguish it from more general multi-directory nesting.

## Why the report should still not be ignored

Even if the current `create()` stack is likely conservative, `tmpfs` still needs an explicit multi-directory locking discipline.

The current `MemoryNode::rename()` implementation no longer holds `src_dir.entries` while locking `dst_dir.entries`; it removes the source entry, drops the source directory lock, and then locks the destination directory. That avoids the immediate two-directory ABBA pattern, but it also leaves the operation non-atomic, as the local `TODO: atomicity` comment notes.

If `rename()` is later made atomic by holding two directory-entry locks at once, it will need either:

- a stable ordering rule, such as inode number or lock address order; or
- explicit lockdep nesting/subclass annotations when the locking relation is structurally one-way.

The current report is therefore useful as a design warning:

- this exact `create()` stack is likely not a real ABBA instance;
- the directory locking model still lacks a documented rule for nested directory-entry locks;
- a future atomic `rename()` implementation can reintroduce a genuine two-directory inversion unless the order is explicit.

## Current conclusion

Current assessment:

- The observed `tmp.rs:388 -> tmp.rs:174` report is more likely a conservative `lockdep` report than a true deadlock in that exact path.
- It should not be dismissed as "pure lockdep noise", because the same directory-entry locking model already permits real two-directory inversion scenarios elsewhere.

In short:

```text
This stack is likely a false positive for the specific create path,
but it still exposes a real locking-design problem in tmpfs.
```

## Follow-up directions

Reasonable next steps are:

1. Remove avoidable nested directory-entry locking in `create()` if possible.
2. Add a stable ordering rule for any path that may take two directory `entries` locks, especially `rename()`.
3. Re-run the Starry `lockdep` configuration after the locking order is explicit.

Possible implementation directions:

- create the child inode without holding the parent `entries` lock across child directory initialization;
- or introduce a stable order, such as ordering by inode number or lock address, before taking two directory-entry locks;
- or extend lockdep expressiveness with nesting/subclass information if the filesystem intentionally relies on structured parent/child lock nesting.

## Subclass experiment plan

The next experiment should make lockdep able to distinguish an ordinary acquisition of a lock class from a structurally nested acquisition of the same base class.

Proposed model:

- keep the default lockdep behavior unchanged by treating all existing lock acquisitions as subclass `0`;
- add an acquire-time subclass parameter, so a single lock instance can be acquired as `(base class, subclass)` depending on the current locking role;
- use `(base class key, subclass)` as the effective dependency-graph node;
- keep the held-lock stack and snapshots storing only the effective dense `class_id`;
- encode the subclass in the class registry key rather than adding a field to `HeldLock`;
- keep order checks real: if lockdep observes `subclass 0 -> subclass 1`, then a later `subclass 1 -> subclass 0` acquisition should still report an inversion.

This is not a suppression mechanism. It is a way to express an intentional one-way nesting relation while preserving the ability to catch the reverse relation.

The representation matters because `HeldLockStack`, `HeldLockSnapshot`, and
`PreparedAcquire` are on hot paths and can affect task memory or function stack
usage. The initial prototype stored `subclass: u32` directly in `HeldLock`,
which increased each held-lock entry from 24 bytes to 32 bytes on 64-bit
targets. With 32 fixed slots, that enlarged each per-task held-lock stack and
each temporary held-lock snapshot by 256 bytes, and also enlarged
`PreparedAcquire` by roughly the same amount.

The revised model therefore treats `class_id` as the effective graph node for
`(base class, subclass)`, but packs the subclass into the registry key:

```text
packed_class_key = (base Location pointer) | subclass
```

The dependency graph still uses dense class IDs, so `MAX_LOCKS` and the
reachability matrix do not grow. The only extra capacity consumed is one class
ID for each actually observed `(base class, subclass)` pair. A unit test locks
the 64-bit sizes back to the previous layout:

```text
HeldLock          = 24 bytes
HeldLockStack     = 776 bytes
HeldLockSnapshot  = 776 bytes
PreparedAcquire   = 792 bytes
```

For the first prototype:

1. Extend `components/lockdep` with nested acquire helpers that accept a `subclass` argument.
2. Keep existing `prepare_acquire_with_snapshot*()` APIs as wrappers using subclass `0`.
3. Add an `ax-sync` helper for `Mutex` nested acquisition, because `lock_api::RawMutex::lock()` itself has no parameter slot for subclass.
4. Use subclass `1` for tmpfs directory-entry locks that are taken while another directory-entry lock is already held, such as parent directory `entries` -> freshly created child directory `entries`.
5. Add a regression test that verifies same-base-class `0 -> 1` is allowed, but `1 -> 0` is rejected after the forward edge has been recorded.

Expected outcome:

- the tmpfs `create()` path no longer trips lockdep solely because parent and child directory-entry mutexes share the same base class;
- lockdep remains able to catch real reverse ordering of the same base class;
- future atomic multi-directory operations, especially `rename()`, still need a stable lock ordering rule rather than relying on subclass annotations alone.

## Experiment result

A first subclass prototype was implemented with the following shape:

- `components/lockdep` accepts an acquire-time subclass and uses `(base class key, subclass)` as the effective dependency graph node.
- `HeldLock`, `HeldLockStack`, `HeldLockSnapshot`, and `PreparedAcquire` do not store a separate subclass field; they continue to carry the effective dense `class_id`.
- The class registry remains a fixed class table; subclass values are encoded
  in the low bits of the packed class key.
- Panic diagnostics recover subclass values from the class registry only for the requested lock and the held-lock snapshot being printed; the hot-path held-lock structures do not grow.
- Existing lockdep acquire APIs remain default-subclass wrappers.
- `ax-sync` exposes `LockdepMutexExt::lock_nested(subclass)`, which degrades to a normal lock when the `lockdep` feature is disabled.
- `tmpfs` uses subclass `1` only for the child directory `entries` lock taken while `MemoryNode::create()` still holds the parent directory `entries` lock.
  It also uses the same nested subclass for the child directory nonempty check in `unlink()`, where the parent directory `entries` lock is still held.

The local verification for the revised packed-key implementation is:

```text
cargo test -p ax-lockdep
cargo xtask clippy --package ax-lockdep
cargo xtask clippy --package ax-sync
cargo xtask clippy --package ax-kspin
cargo xtask clippy --package starry-kernel
```

All of the above checks passed.

The targeted reproducer was rerun:

```text
TMPDIR=/tmp FEATURES=lockdep cargo xtask starry test qemu --arch riscv64 -g normal -c test-shm-deadlock
```

The previous tmpfs report:

```text
os/StarryOS/kernel/src/pseudofs/tmp.rs:388 -> os/StarryOS/kernel/src/pseudofs/tmp.rs:174
```

did not recur after the subclass annotation.

The run now stops later on a different lockdep report:

```text
lockdep: lock order inversion detected
requested:
  kind=spin lock ... acquire_at=components/starry-process/src/process.rs:214:51
conflicting held lock:
  ... acquired_at=components/starry-process/src/process.rs:211:42
```

That new report is in `Process::exit()`:

```text
self.children.lock() -> reaper.children.lock()
```

Both locks are `Process::children` locks from different `Process` instances and still use the default subclass `0`.

Current experiment conclusion:

- subclass support is sufficient to express the tmpfs parent -> freshly-created-child nesting and removes the original conservative report;
- the broader lockdep run exposed another same-class multi-instance nesting in `starry-process`;
- `Process::exit()` did not need a subclass annotation: it was restructured to move `self.children` into a local map, drop the `self.children` lock, then lock `reaper.children`;
- after that restructuring, the `starry-process` lockdep report no longer appears in the targeted QEMU run.

The current targeted QEMU run now reaches the shell and fails later on a
different diagnostic:

```text
stack overflow/corruption detected for Task(1, "idle"):
stack=[0xffffffc080520000..0xffffffc080524000), expected magic=0x57acce1157acce11
```

That is no longer a lockdep order report. It points at the 16 KiB primary idle
task stack in `os/arceos/modules/axtask/src/run_queue.rs`. Further work should
separate that stack-canary issue from the subclass implementation.

## Instance identity cleanup

The first lockdep implementation also assigned each `LockdepMap` a global
monotonic `instance_id`. That was intended to model Linux lockdep's lock
instance identity, but it is not equivalent to Linux's representation.

Linux stores the actual `lockdep_map *` in each held lock and compares that
pointer to match a concrete lock object. It does not allocate a finite global
ID for every lock object. The previous Starry/ArceOS implementation therefore
made dynamic lock objects consume the same `MAX_LOCKS` capacity used by lock
classes and could panic after enough dynamic lock instances were observed.

The revised implementation keeps instance identity but uses the lock object's
address directly:

- held-lock entries retain the concrete lock address;
- recursive acquisition detection compares the requested lock address with
  addresses already held by the task;
- release tracking pops by lock address;
- `LockdepMap` now stores only class-related state.

This removes the artificial tracked-instance limit without increasing the
held-lock stack or snapshot size. A regression test creates more dynamic lock
objects than the class table size and verifies that they do not consume class
slots merely by existing as distinct instances.

## Follow-up: intermittent `test-shm-deadlock` timeout

After the tmpfs and `starry-process` lockdep reports were fixed, another
targeted QEMU run was observed to intermittently time out:

```text
TMPDIR=/tmp FEATURES=lockdep cargo xtask starry test qemu --arch riscv64 -g normal -c test-shm-deadlock
```

The reproduced failure pattern is:

- the test prints the first seven PASS lines through `clone shmget_thread`;
- it does not print the final `no deadlock detected` PASS line;
- it does not print a lockdep report;
- it does not print a panic or stack-canary diagnostic;
- the QEMU harness eventually reports `QEMU timed out after 120s`.

The relevant C test code is:

```text
usleep(SHM_RACE_USEC);
g_running = 0;
waitpid(tid1, &status, __WALL);
waitpid(tid2, &status, __WALL);
waitpid(tid3, &status, __WALL);
CHECK(!g_deadlock_detected, "no deadlock detected");
```

This means the timeout happens before the final CHECK and most likely while the
main test thread is waiting for one of the cloned workers to exit. The watchdog
thread is not conclusive in this phase: once the main thread sets
`g_running = 0`, the watchdog exits cleanly, so a worker stuck in the kernel can
still lead to a harness timeout without printing the test's `FAIL` line.

Two non-lockdep control runs of the same command without `FEATURES=lockdep`
completed successfully. That is useful data, but not enough to prove that
lockdep itself is the cause; enabling lockdep changes timing and can expose an
existing SMP race or an exit/SHM cleanup issue.

Current status:

- this intermittent timeout is recorded as an unresolved follow-up;
- it is not currently attributed to the lockdep subclass or instance-identity
  changes;
- the lockdep cleanup should proceed separately from this possible
  `test-shm-deadlock` runtime race.

## Follow-up: FAT32/VFS lockdep visibility gap

There is another possible filesystem lock-order issue that is not currently
covered by lockdep.

The relevant FAT32 implementation uses the project-local kernel spin lock:

```text
os/arceos/modules/axfs-ng/src/fs/fat/fs.rs:
  use ax_kspin::{SpinNoPreempt as Mutex, SpinNoPreemptGuard as MutexGuard};
```

So the FAT filesystem lock is visible to lockdep when `ax-kspin/lockdep` is
enabled.

At the time of this analysis, the VFS layer still imported the third-party
`spin` crate directly:

```text
components/axfs-ng-vfs/src/lib.rs:
  use spin::{Mutex, MutexGuard};
```

That dependency was already present when `axfs-ng-vfs` was imported as a
subtree:

```text
components/axfs-ng-vfs/Cargo.toml:
  spin = { version = "0.10", default-features = false, features = ["mutex"] }
```

That `spin::Mutex` use has since been migrated to project-local `ax_kspin`
locks. `ax-kspin` is related but not identical to the external `spin` crate:
its `BaseSpinLock` is explicitly based on `spin::Mutex`, but it is a separate
project-local implementation that adds kernel guard semantics and lockdep
acquire/release hooks.

Before that migration, this created a lockdep blind spot:

- `ax_kspin::SpinNoPreempt` locks are visible to lockdep;
- `spin::Mutex` locks in `axfs-ng-vfs` were not visible to lockdep;
- any dependency edge involving a VFS `spin::Mutex` therefore could not be
  recorded.

The suspected FAT32/VFS ordering is:

```text
DirNode.cache lock -> FAT filesystem lock
FAT filesystem lock -> DirNode.cache lock
```

The first direction can happen through the VFS cached lookup path:

```text
DirNode::lookup()
  -> self.cache.lock()
  -> lookup_locked()
  -> self.ops.lookup(name)
  -> FAT lookup takes the FAT filesystem lock
```

The second direction can happen through FAT directory iteration:

```text
FatDirNode::read_dir()
  -> self.fs.lock()
  -> dir_node.lookup_cache(name) / dir_node.insert_cache(name, entry)
  -> DirNode.cache lock
```

This is a real ABBA-shaped ordering risk, not a subclass problem like the tmpfs
parent/child entry lock report. The reason current lockdep may not report it is
that one side of the pair, `DirNode.cache`, is outside the lockdep-visible lock
set.

Historical implication:

- a missing lockdep report did not prove the FAT32/VFS ordering was safe;
- lockdep coverage was incomplete for `axfs-ng-vfs` internals before the
  migration;
- FAT32-specific testing may also be absent from the normal Starry QEMU path,
  so the code path might not be exercised even if all locks were visible.

The migration was not treated as a mechanical rename. The chosen lock type must
continue to match the context:

- `SpinNoPreempt` is probably the first candidate for VFS cache locks if they
  are only used in task context;
- `SpinNoIrq` may be needed only for locks that can be taken from IRQ-enabled
  contexts where interrupt-side reentry is possible;
- `SpinRaw` should remain reserved for contexts that already guarantee the
  necessary preemption/IRQ state externally.

Before changing the VFS lock type, the code paths that access `DirNode.cache`
and `DirEntry` user data should be checked for:

- use from interrupt context;
- use while holding block-device or filesystem implementation locks;
- nested VFS operations through callbacks such as directory-entry sinks;
- layout or dependency impact on `axfs-ng-vfs`, which is a component crate and
  not only a StarryOS-local module.

After such a migration, the FAT32/VFS ordering should be retested with a
targeted FAT32 case. If lockdep then reports the suspected ABBA, the fix should
be a real ordering change, not a subclass annotation: either avoid holding the
VFS cache lock across filesystem backend callbacks, or establish a single
stable order between VFS cache locks and filesystem implementation locks.
