# Starry stable PID and zombie identity

## Status

This is a work-in-progress handoff for the Starry process-lifecycle repair.
The branch intentionally records the current implementation and deterministic
regression before the wider `ax-task`/`ax-runtime` migration continues. It has
not been brought to a green build or test state.

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
    credential and wait metadata
}
        |
        | exactly one consuming wait removes the matching Arc identity
        v
Reaped
```

The numeric PID is only a lookup key. `Arc<Process>` is the generation-specific
identity, and `Arc::ptr_eq` is required whenever a long-lived pidfd or waiter
validates a lookup result.

The lifecycle split is deliberate:

- `ProcessData` owns live runtime resources such as address space and file
  tables;
- `Process` owns the stable Linux-visible identity and parent/group links;
- the PID registry owns either the live weak resource reference or the zombie
  snapshot, never both;
- `PollSet` follows the stable identity from live construction through zombie
  and reap.

## Current patch

The WIP patch:

- changes `Process` from an `AtomicBool` zombie flag to
  `Live -> Zombie -> Reaped` atomic transitions;
- makes reap a unique transition and removes only exact parent/group identities;
- replaces the separate live and zombie registries with one typed PID registry;
- resolves `pidfd_open` from one registry lock and retains `Arc<Process>`;
- shares the original process exit event with pidfds opened after exit;
- reports `IN | RDNORM` for zombies and `IN | RDNORM | HUP` after reap;
- wakes `RDNORM` on exit and `HUP` on the unique reap path;
- copies wait status to userspace before attempting the consuming transition;
- changes the QEMU case to prove zombie state with
  `waitid(P_PID, ..., WEXITED | WNOWAIT | WNOHANG)`.

No driver, scheduler, or `ax-task` API changes belong in this branch.

## Red evidence

Command:

```bash
cargo xtask starry test qemu --arch x86_64 \
  -c qemu/system/syscall-test-pidfd-send-signal
```

On unmodified `ee68a61f2` plus only the regression test, the formal runner ended
with `STARRY_GROUPED_TESTS_FAILED`: 71 assertions passed and 7 failed.

The deterministic failures were:

- `EPOLLRDNORM`-only interest did not observe an unreaped zombie;
- the returned event omitted `EPOLLRDNORM`;
- a waiter registered for `EPOLLHUP` before `waitpid` timed out;
- reap did not publish `EPOLLHUP`;
- post-reap polling/epoll omitted the Linux event mask.

This is fail-first evidence only. The implementation in this branch has not
been rebuilt or rerun after being copied into the clean branch.

## Required continuation work

The next contributor should proceed in this order:

1. run `cargo fmt --all` and resolve syntax/import issues without changing the
   lifecycle design;
2. run `cargo xtask clippy --package starry-process`, then
   `cargo xtask clippy --package starry-kernel`;
3. rerun the exact QEMU command above and preserve the full formal success or
   failure marker;
4. add a host-level concurrent reap test proving only one waiter wins;
5. audit the lock order around `publish_zombie`: it currently holds the global
   PID registry while `Process::exit` takes process child locks;
6. verify CPU-time accounting is snapshotted before live task resources can
   disappear and credited only by the winning waiter;
7. add a multi-thread group-leader test matching Linux
   `delay_group_leader()` readiness;
8. run the same userspace regression on host Linux and then on all supported
   Starry architectures.

Known adjacent gaps that should remain separate unless needed to make the core
identity correct:

- `waitid(P_PIDFD)` does not yet map `PIDFD_NONBLOCK` to nonblocking wait
  semantics;
- `pidfd_getfd` still uses a kill-style permission approximation instead of
  Linux `ptrace_may_access(PTRACE_MODE_ATTACH_REALCREDS)`;
- thread-pidfd `siginfo` self checks need TID/TGID review;
- early thread-group-leader exit needs explicit delayed-readiness coverage;
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
| `getpgid` / `getsid` | The stable `Process` remains visible through zombie and disappears at reap. | [`find_task_by_vpid` users](https://github.com/torvalds/linux/blob/1590cf0329716306e948a8fc29f1d3ee87d3989f/kernel/sys.c#L1187-L1212) |
