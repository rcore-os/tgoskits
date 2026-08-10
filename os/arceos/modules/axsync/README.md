# axsync

[![Crates.io](https://img.shields.io/crates/v/axsync)](https://crates.io/crates/axsync)
[![Docs.rs](https://docs.rs/axsync/badge.svg)](https://docs.rs/axsync)

[ArceOS](https://github.com/arceos-org/arceos) synchronization primitives.

## Primitives

- **SpinMutex**: A non-sleeping, IRQ-safe `ax_kspin::SpinNoIrq` lock.
- **PiMutex**: With `multitask`, an urgency-ordered sleeping mutex with targeted ownership handoff. It reports ownership and wait edges to `ax-task`, which owns transitive donation, scheduler requeue, and Deadline donor-budget semantics.
- **spin**: Re-export of the [ax-kspin](https://crates.io/crates/ax-kspin) crate (spinlocks).

## Features

- `multitask`: Enable the task scheduler's PI mutex protocol. `ax-task`'s per-lock `PiMutexCore` is the single owner of the physical owner word and urgency-ordered waiter tree; waiter linkage lives in the blocked thread. Registration, donation, deboost, and handoff are committed in one scheduler transaction, while blocking and targeted wake run after all metadata gates have been released.
- `lockdep`: Enable sleeping-lock dependency validation in addition to PI.

## License

This project is licensed under GPL-3.0-or-later OR Apache-2.0 OR MulanPSL-2.0.
