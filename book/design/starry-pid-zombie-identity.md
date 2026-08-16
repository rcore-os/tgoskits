# Starry stable PID and zombie identity

## Status

This document records the implemented Starry process-lifecycle repair and its
deterministic regression evidence. The lifecycle is now owned by one stable PID
identity state machine; `Process` is limited to thread-group and relationship
topology.

Base: `origin/dev` at `ee68a61f2`.

## Problem

Starry currently models one Linux process with two independently maintained
registries:

- `PROCESS_TABLE` contains a weak `ProcessData` reference while runtime
  resources are alive;
- `ZOMBIE_TABLE` contains a separate strong `Process` snapshot from final exit
  until wait consumes the zombie.

A pidfd opened while the process is live stores the `ProcessData` event, while a
pidfd opened after exit creates a new private event. This creates several
observable inconsistencies:

1. a numeric PID lookup can cross the live/zombie boundary through two locks;
2. the pidfd does not itself retain the generation-specific `Process` identity;
3. `epoll` interested only in `EPOLLRDNORM` misses zombie readiness because the
   file reports only `POLLIN`;
4. reap removes the zombie entry but does not wake pidfds already waiting for
   `EPOLLHUP`;
5. concurrent waiters can both execute non-atomic `free`/registry cleanup;
6. an old pidfd can accidentally validate against a reused numeric PID unless
   the exact process object is checked.

The previous test used `kill(child, 0)` as its zombie proof. That only proves
that the PID exists, so it did not force the process to have exited without
being reaped and could not exercise these state transitions deterministically.

## Linux and rt-linux reference

The comparison target is Linux `v7.2-rc4`, commit
`1590cf0329716306e948a8fc29f1d3ee87d3989f`. The current PREEMPT_RT release is
`7.2-rc4-rt3`; its patch does not modify `include/linux/pid.h`, `fs/pidfs.c`,
`kernel/exit.c`, or `kernel/signal.c`, so the mainline lifecycle below also
applies to that rt-linux version.

- [`struct pid` is reference-counted and owns `wait_pidfd`](https://github.com/torvalds/linux/blob/1590cf0329716306e948a8fc29f1d3ee87d3989f/include/linux/pid.h#L35-L75).
  Numeric PID reuse allocates a different object, so a pidfd cannot suffer ABA.
- [`pidfd_poll`](https://github.com/torvalds/linux/blob/1590cf0329716306e948a8fc29f1d3ee87d3989f/fs/pidfs.c#L305-L323)
  returns `EPOLLIN | EPOLLRDNORM` for an observable exited task and additionally
  returns `EPOLLHUP` after the task is detached during reap.
- [`do_notify_pidfd`](https://github.com/torvalds/linux/blob/1590cf0329716306e948a8fc29f1d3ee87d3989f/kernel/signal.c#L2158-L2166)
  publishes exit readiness.
- [`__unhash_process`](https://github.com/torvalds/linux/blob/1590cf0329716306e948a8fc29f1d3ee87d3989f/kernel/exit.c#L132-L145)
  detaches the task and wakes the stable pidfd wait queue at reap.
- [`wait_task_zombie`](https://github.com/torvalds/linux/blob/1590cf0329716306e948a8fc29f1d3ee87d3989f/kernel/exit.c#L1207-L1250)
  leaves `WNOWAIT` non-consuming and uses an atomic state transition for the
  consuming waiter.
- [`pidfd_send_signal`](https://github.com/torvalds/linux/blob/1590cf0329716306e948a8fc29f1d3ee87d3989f/kernel/signal.c#L4020-L4058)
  resolves the stable PID object. It returns `ESRCH` after reap; before reap,
  signal zero and a permitted nonzero signal can resolve the zombie identity
  without changing the recorded exit status.
- rt-linux release index:
  <https://www.kernel.org/pub/linux/kernel/projects/rt/7.2/>.

## Intended state model

```text
Live(Weak<ProcessData>)
        |
        | final thread publishes exit under PID-registry write lock
        v
Zombie {
    Arc<Process>,
    Arc<PollSet>,
    credential, wait metadata, and frozen CPU time
}
        |
        | exactly one consuming wait claims the matching Arc identity
        v
Reaping
        |
        | retire parent/group topology while the PID remains registry-reserved
        | but is no longer externally openable
        v
Reaped
```

The numeric PID is only a lookup key. `Arc<Process>` is the generation-specific
identity, and `Arc::ptr_eq` is required whenever a long-lived pidfd or waiter
validates a lookup result.

The lifecycle split is deliberate:

- `ProcessData` owns live runtime resources such as address space and file
  tables;
- `ProcessIdentity` owns the stable Linux-visible identity and lifecycle;
- `Process` owns thread-group state and parent/group topology;
- the PID registry owns either the live weak resource reference or the zombie
  snapshot, never both;
- `PollSet` follows the stable identity from live construction through zombie
  and reap.

## Implemented patch

The patch:

- removes lifecycle flags from `starry-kernel::task::Process` and introduces the
  typed kernel-owned
  `Live -> Zombie -> Reaping -> Reaped` identity state machine;
- constructs the stable identity together with `ProcessData`, so clone-created
  pidfds and later registry publication cannot diverge or publish a failed
  clone early;
- makes final thread exit a one-shot operation and atomically accumulates each
  exiting thread's CPU time under the thread-group lock;
- freezes credentials, wait metadata, and process CPU time in the zombie
  snapshot;
- makes reap a unique claim, credits child CPU time only to the winning waiter,
  and removes only exact parent/group identities;
- retains the identity in `Reaping` until topology retirement finishes, so the
  numeric PID cannot be reused in the middle of cleanup;
- treats that `Reaping` registry entry only as an internal PID reservation:
  ordinary numeric-PID lookup, process/thread `pidfd_open`, and pidfd signal
  target resolution accept only `Live | Zombie`;
- holds the PID-registry read lock through the public-visibility state check,
  which linearizes lookup against the write-locked `Zombie -> Reaping` claim;
- replaces the separate live and zombie registries with one typed PID registry;
- resolves `pidfd_open` from one registry lock and retains
  `Arc<ProcessIdentity>`;
- shares the original process exit event with pidfds opened after exit;
- reports `IN | RDNORM` for zombies and `IN | RDNORM | HUP` after reap;
- wakes `RDNORM` on exit and `HUP` on the unique reap path;
- copies wait status to userspace before attempting the consuming transition;
- keeps `WNOWAIT` observational and makes pidfd waits match an exact PID
  generation rather than a reused numeric PID;
- covers leader-before-worker exit, frozen CPU accounting, and concurrent
  waiter ownership in the QEMU regression.

No driver, scheduler, or `ax-task` API changes belong in this branch.

## Red evidence

Command:

```bash
cargo xtask starry test qemu --arch x86_64 \
  -c qemu/system/syscall-test-pidfd-send-signal
```

On unmodified `ee68a61f2` plus only the original pidfd regression, the formal
runner ended with `STARRY_GROUPED_TESTS_FAILED`: 71 assertions passed and 7
failed.

The deterministic failures were:

- `EPOLLRDNORM`-only interest did not observe an unreaped zombie;
- the returned event omitted `EPOLLRDNORM`;
- a waiter registered for `EPOLLHUP` before `waitpid` timed out;
- reap did not publish `EPOLLHUP`;
- post-reap polling/epoll omitted the Linux event mask.

Two continuation regressions were also proven red against the previous
implementation:

- `repeated_thread_exit_does_not_report_last_twice` showed that removing an
  already-removed TID incorrectly reported the last-thread transition again;
- `qemu/system/syscall-test-waitid-pidfd` ended with 55 passes and one failure
  because a consumed child contributed no frozen CPU time to its parent.

The review regression `reaping_identity_is_not_publicly_resolvable` adds a
test-only barrier immediately after the consuming waiter claims
`Zombie -> Reaping`.
Before the public-visibility filter, `get_process`, `getpgid`, and `getsid`
still resolved the already-consumed PID in that deterministic window. The
axtest reported `not ok` alongside the unrelated pre-existing
`task_clone_validation_rules_hold` failure: 397 passed and 2 failed.

## Validation evidence

- the former `starry-process` topology tests are now kernel task tests; the
  standalone package and its old validation commands no longer exist;
- `cargo xtask clippy --package starry-kernel`: 22/22 feature checks passed;
- `reaping_identity_is_not_publicly_resolvable` now reports `ok` under
  `cargo xtask ktest qemu -p starry-kernel --arch x86_64`; the complete local
  axtest run reports 398 passes and one unrelated pre-existing
  `task_clone_validation_rules_hold` failure;
- the waitid/pidfd userspace test passed on host Linux with 66/66 assertions;
- `cargo xtask starry test qemu --arch x86_64
  -c qemu/system/syscall-test-waitid-pidfd`: 66/66 assertions passed and the
  formal runner emitted `STARRY_GROUPED_TESTS_PASSED`.
- `cargo xtask starry test qemu --arch x86_64
  -c qemu/system/syscall-test-pidfd-send-signal`: 78/78 assertions passed,
  including zombie readiness and post-reap `EPOLLHUP`.

The remaining architecture matrix is delegated to CI; no physical-board path
is required for this process-lifecycle-only change.

The `Reaping` window cannot be controlled from userspace without exposing an
internal topology lock, so a pthread race in the syscall suite would be
probabilistic. Existing QEMU cases retain the syscall-level pidfd/wait coverage,
while the kernel axtest supplies deterministic concurrent coverage for this
internal linearization point.

Known adjacent gaps that should remain separate unless needed to make the core
identity correct:

- `waitid(P_PIDFD)` does not yet map `PIDFD_NONBLOCK` to nonblocking wait
  semantics;
- `pidfd_getfd` still uses a kill-style permission approximation instead of
  Linux `ptrace_may_access(PTRACE_MODE_ATTACH_REALCREDS)`;
- thread-pidfd `siginfo` self checks need TID/TGID review;
- PID namespaces and allocator reuse need an end-to-end ABA stress test.

## Syscall impact map

| Syscall | Intended behavior in this patch | Reference |
| --- | --- | --- |
| `pidfd_open` | Resolve one live or zombie generation atomically; fail after reap. | [`pidfd_prepare`](https://github.com/torvalds/linux/blob/1590cf0329716306e948a8fc29f1d3ee87d3989f/fs/pidfs.c#L680-L728) |
| `pidfd_send_signal` | Zombie identity remains resolvable until reap; old pidfd returns `ESRCH` after reap. | [`do_pidfd_send_signal`](https://github.com/torvalds/linux/blob/1590cf0329716306e948a8fc29f1d3ee87d3989f/kernel/signal.c#L4020-L4058) |
| `wait4` / `waitpid` | Copy status first, then let one waiter consume the zombie. | [`wait_task_zombie`](https://github.com/torvalds/linux/blob/1590cf0329716306e948a8fc29f1d3ee87d3989f/kernel/exit.c#L1207-L1250) |
| `waitid` | `WNOWAIT` observes without consuming; normal wait performs the unique reap. | [`wait_task_zombie`](https://github.com/torvalds/linux/blob/1590cf0329716306e948a8fc29f1d3ee87d3989f/kernel/exit.c#L1207-L1250) |
| `poll` / `ppoll` | Zombie reports `POLLIN | POLLRDNORM`; reaped pidfd also reports `POLLHUP`. | [`pidfd_poll`](https://github.com/torvalds/linux/blob/1590cf0329716306e948a8fc29f1d3ee87d3989f/fs/pidfs.c#L305-L323) |
| `epoll_wait` | `EPOLLRDNORM` interest is independently observable and reap wakes `EPOLLHUP` waiters. | [`pidfd_poll`](https://github.com/torvalds/linux/blob/1590cf0329716306e948a8fc29f1d3ee87d3989f/fs/pidfs.c#L305-L323) |
| `getpgid` / `getsid` | The stable `Process` remains visible through zombie and returns `ESRCH` once reap is claimed. | [`find_task_by_vpid` users](https://github.com/torvalds/linux/blob/1590cf0329716306e948a8fc29f1d3ee87d3989f/kernel/sys.c#L1187-L1212) |
