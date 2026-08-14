# StarryOS cgroup v2 pids L4 design

## Problem and success criteria

The current `ax-cgroup` crate owns hierarchy, process membership, and cgroup
namespace state, but its `cgroup.controllers` and `cgroup.subtree_control`
interfaces are empty. Consequently, `clone(2)` can always create a task and
the existing cgroup QEMU test cannot distinguish an enforced limit from a
no-op implementation.

This change adds the first controller increment: pids. A successful
implementation lets a process move into a non-root pids-enabled cgroup, set
`pids.max`, and observe that the next task-creating `fork` or `clone` fails
with `EAGAIN` once the hierarchical limit is exhausted. The counters must
remain balanced across fork rollback, thread exit, process exit, and process
migration.

This is deliberately not a CPU, memory, cpuset, or I/O implementation. It
does not add cgroup v2 threaded mode, delegation, notifications,
`pids.events.local`, `pids.peak`, controller disabling, or the cgroup core's
no-internal-process rule. Those features require their own observable behavior
and validation.

## Prior art and alternatives

The semantics follow Linux v6.12 at commit
`adc218676eef25575469234709c2d87185ca223a`, especially
[`cgroup-v2.rst`](https://github.com/torvalds/linux/blob/adc218676eef25575469234709c2d87185ca223a/Documentation/admin-guide/cgroup-v2.rst#L2251-L2285),
[`pids_try_charge()` and rollback](https://github.com/torvalds/linux/blob/adc218676eef25575469234709c2d87185ca223a/kernel/cgroup/pids.c#L166-L209),
and
[`pids_event()`](https://github.com/torvalds/linux/blob/adc218676eef25575469234709c2d87185ca223a/kernel/cgroup/pids.c#L243-L271):

- pids tracks tasks (kernel TIDs), not only process leaders;
- `pids.current` includes the cgroup's descendants;
- organizational operations may exceed `pids.max`; only fork and clone are
  rejected with `EAGAIN`;
- pids limits are hierarchical and `pids.events` records failures in the
  cgroup's subtree.

`fork/feat/cgroup-unified-rebased` already contains a controller framework
and pids implementation. It is retained as a behavioral reference, but it is
not merged or cherry-picked because it replaces the current cgroup crate and
pulls `axfs-ng-vfs` and `ax_errno` into the reusable domain layer. The chosen
approach extends the current `CgroupNode` and `CgroupProvider` boundaries and
keeps VFS text rendering and Linux errno conversion in StarryOS kernel glue.

Open draft PR [#1379](https://github.com/rcore-os/tgoskits/pull/1379) has
partial surface overlap because it also contains a pids controller among five
controllers, manager/inotify work, and scheduler changes. It is classified as
`conflict-risk/partial-overlap`, not as a reusable replacement: its pids
accounting is process-based and does not provide this increment's task-level
thread accounting, migration serialization, reservation rollback, or
`CLONE_PARENT_SETTID` rejection regression.

## State, ownership, and synchronization

Every `CgroupNode` owns a private pids state. The state retains a task count,
an optional maximum, and the hierarchical `pids.events` max counter. Counts
and max events are maintained for the affected non-root ancestors so they
match the cgroup v2 hierarchy before an interface becomes visible.
The root owns accounting state but does not expose `pids.*` files; those files
exist only in non-root cgroups whose parent enabled pids in
`cgroup.subtree_control`.

`MembershipState` is the serialization boundary for task lifecycle changes. It
owns pending task reservations and the committed task-to-cgroup mapping. A
fork guard charges the source cgroup path before the child is runnable and
either commits the mapping or releases every charge on drop. A task exit
removes exactly one committed mapping before uncharging its path. Process
migration receives all live TIDs from `CgroupProvider`, moves their mappings
as one membership transaction, charges the target-only path without checking
limits, and releases the source-only path after publication.

The per-node pids counter uses compare-and-exchange for limit-checked fork
charges. The membership lock orders multi-node lifecycle operations and
prevents migration from observing a partially committed task set. No VFS,
allocation, scheduling, or provider callback executes while a node counter is
being updated.

## Interfaces and errors

`CgroupProvider` gains a live-task query so migration can move a complete
thread group. The kernel implementation obtains that snapshot from
`starry-process::Process::threads()`; test providers model it explicitly.

The domain API distinguishes a process child from a thread child. Both reserve
a pids task charge, while only a process child is added to `cgroup.procs`.
Task exit takes the task identity and whether it is the last task in the
process, so membership remains process-based while pids accounting remains
task-based.

`CgroupError::LimitExceeded` is the domain result for a failed pids charge and
the StarryOS cgroup adapter maps it to `EAGAIN`. Invalid pids input and
unsupported controller operations map to `EINVAL`; a top-down controller
violation or a pending lifecycle operation maps to `EBUSY`.

## User-visible syscall impact

The implementation does not change syscall numbers, argument layouts, or
flag parsing. It changes the state transitions reached through the following
existing entry points:

| Syscall | Impact and compatibility basis |
| --- | --- |
| `clone` | `CloneArgs::do_clone` reserves one pids charge before publishing the TID and returns `EAGAIN` on a hierarchical limit, matching [`clone(2)`](https://man7.org/linux/man-pages/man2/clone.2.html) and Linux [`pids_can_fork()`](https://github.com/torvalds/linux/blob/adc218676eef25575469234709c2d87185ca223a/kernel/cgroup/pids.c#L273-L284). A `CLONE_PARENT_SETTID` pointer is written only after the reservation succeeds, so a rejected clone has no parent-TID side effect. |
| `clone3` | Uses the same `CloneArgs::do_clone` path after existing ABI validation, so the pids result is identical to `clone`; argument parsing is unchanged. |
| `fork` | The architecture wrapper uses the clone path with `SIGCHLD`; the pids rejection is therefore `EAGAIN`, matching [`fork(2)`](https://man7.org/linux/man-pages/man2/fork.2.html). |
| `vfork` | The architecture wrapper uses the clone path with `CLONE_VFORK | CLONE_VM`; the charge is committed before publication and before the parent waits, matching [`vfork(2)`](https://man7.org/linux/man-pages/man2/vfork.2.html). |
| `execve` | Successful de-threading renames the surviving task ledger entry without changing the charge; executable loading, argument handling, and errno ordering are unchanged. See [`execve(2)`](https://man7.org/linux/man-pages/man2/execve.2.html). |
| `execveat` | Shares the same `do_execve` de-thread path as `execve`; only the internal task identity mapping is updated. See [`execveat(2)`](https://man7.org/linux/man-pages/man2/execveat.2.html). |
| `exit` | `do_exit` removes exactly one task charge after thread-group bookkeeping; repeated teardown is idempotent. See [`_exit(2)`](https://man7.org/linux/man-pages/man2/_exit.2.html). |
| `exit_group` | The same exit path releases each exiting task and removes process membership on the final task. Signal-driven exits use this same lifecycle boundary. |

The cgroupfs interface is observed through existing `mount`, `openat`, `read`,
`write`, and `getdents64` paths. The change adds dynamic controller and
`pids.*` entries to the existing cgroup2 filesystem adapter; it does not alter
the generic syscall ABI. The QEMU cases validate file visibility, permissions,
input errors, `cgroup.procs` migration, and read-back results.

## Validation evidence

Host tests cover parse and read-back behavior, hierarchical charges, CAS limit
races, rollback, thread accounting, idempotent exit, and migration above a
limit. The Starry QEMU system test performs the user-visible sequence of
enabling pids, migration, `pids.max` update, successful first fork, failed
second fork with `EAGAIN`, a rejected raw `clone(CLONE_PARENT_SETTID)`,
hierarchical event observation, and counter recovery after exit. It must fail
if the clone-path limit check, parent-TID ordering, or event propagation is
removed.

Full validation of the patch completed on base
`106bede3070da3c44fd1cc61190aa95e96149ca6` on 2026-08-13
(Asia/Shanghai). Before the ordering fix, the new aarch64 test produced
56 passes and 2 failures: the rejected clone changed the parent-TID pointer,
and the existing event expectation did not account for the additional valid
rejection. Moving the write after the cgroup reservation commit and updating
the event expectation produced 58 passes and 0 failures.

| Gate | Result |
| --- | --- |
| `cargo fmt --all -- --check` | passed |
| `cargo xtask lock-lint` | passed |
| `cargo test -p ax-cgroup` | 20 passed |
| `cargo xtask clippy --package ax-cgroup` | passed |
| `cargo check -p ax-cgroup` | passed |
| `cargo check -p starry-kernel` | passed |
| `cargo xtask clippy --package starry-kernel` | 25 configurations passed with the full cross-toolchain `PATH` |
| `cargo xtask starry build --arch aarch64` | passed on final worktree |
| `cargo xtask starry build --arch riscv64` | passed on final worktree |
| `cargo xtask starry build --arch loongarch64` | passed on final worktree |
| `cargo xtask starry build --arch x86_64` | passed on final worktree |
| aarch64 `qemu/system/cgroup-pids` | 58 passed, 0 failed |
| riscv64 `qemu/system/cgroup-pids` | 58 passed, 0 failed |
| loongarch64 `qemu/system/cgroup-pids` | passed the same focused binary |
| x86_64 `qemu/system/cgroup-pids` | 58 passed, 0 failed |

The host regression also covers rejecting migration while a pending task is
unpublished and resolving a reservation from the process's current cgroup.
All four supported QEMU architectures were rebuilt and rerun after the final
API convergence. Existing Cargo artifacts, rootfs archives, and grouped-case
assets were reused; no `cargo clean` was run.

The branch was then rebased without conflicts onto
`9063198d91d0fbf42be575c02f5e2bf9e94f2c58`, whose only change from the
validated base adds files under `apps/starry/aka-rk3588`. It does not touch
cgroup, clone, the focused test, Cargo metadata, or the build/test tooling.
The committed patches were checked with `git range-diff`, and the full
cross-build/QEMU evidence was reused rather than rebuilt for this unrelated
baseline-only change. Fast host and kernel gates were rerun on the rebased
worktree before the final local commit.
