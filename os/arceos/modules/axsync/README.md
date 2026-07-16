# axsync

[![Crates.io](https://img.shields.io/crates/v/axsync)](https://crates.io/crates/axsync)
[![Docs.rs](https://docs.rs/axsync/badge.svg)](https://docs.rs/axsync)

[ArceOS](https://github.com/arceos-org/arceos) synchronization primitives.

## Primitives

- **Mutex**: With `multitask`, an urgency-ordered sleeping mutex with targeted ownership handoff. It reports ownership and wait edges to `ax-task`, which owns transitive donation, scheduler requeue, and Deadline donor-budget semantics. Otherwise it is an alias of `ax_kspin::SpinNoIrq`.
- **spin**: Re-export of the [ax-kspin](https://crates.io/crates/ax-kspin) crate (spinlocks).

## Features

- `multitask`: Enable the task scheduler's PI mutex protocol. Short owner and waiter metadata transitions use `ax-kspin`; donation, blocking, handoff, and wake operations run after the metadata lock is released.
- `lockdep`: Enable sleeping-lock dependency validation in addition to PI.

## License

This project is licensed under GPL-3.0-or-later OR Apache-2.0 OR MulanPSL-2.0.
