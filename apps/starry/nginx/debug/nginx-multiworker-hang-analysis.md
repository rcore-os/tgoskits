# Nginx Multi-Worker Hang Analysis (StarryOS)

## Scope

- Scenario: `master_process on; worker_processes 2;` in phase1 lifecycle flow.
- Symptom: startup/request/quit path can stall on StarryOS, Linux control run is stable.

## Root Cause

- `accept4()` in worker goes through `ax-net-ng` `poll_io(...)`, which is wrapped by
  `axtask::future::interruptible(...)`.
- `interruptible(...)` only exits early when the target task carries the interrupt flag
  (`task.interrupt()`), then syscall can return `EINTR` and userspace can process pending signals.
- In Starry signal send path, deliverable signals in `send_signal_to_thread` and
  `send_signal_to_process` used `ax_task::wake_task(...)` only, not `task.interrupt()`.
- Result: worker blocked in `accept4` is wakeable once but not marked interrupted;
  it re-enters pending wait and cannot promptly break for `SIGQUIT`, causing master quit/reap
  instability in multi-worker nginx.

## Fix

- Replace wake-only behavior with interrupt in both delivery paths:
  - `os/StarryOS/kernel/src/task/signal.rs` `send_signal_to_thread`
  - `os/StarryOS/kernel/src/task/signal.rs` `send_signal_to_process`
- Keep the blocked-signal/sigwait special wake path unchanged to avoid unrelated `EINTR` noise.

## Why This Matches The Symptom

- Multi-worker nginx has workers frequently blocked in `accept4` on shared listen fd.
- Quit path relies on signal-driven unblock and graceful worker exit.
- Without interrupt flag, blocking poll-based syscall does not break deterministically.
- After switching to `task.interrupt()`, blocking accept path can return `EINTR` and
  worker exits follow expected signal semantics.
