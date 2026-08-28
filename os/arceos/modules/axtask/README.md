# ax-task

[![Crates.io](https://img.shields.io/crates/v/ax-task)](https://crates.io/crates/ax-task)
[![Docs.rs](https://docs.rs/ax-task/badge.svg)](https://docs.rs/ax-task)

[ArceOS](https://github.com/arceos-org/arceos) task management module.

This module provides primitives for task management, including task creation,
scheduling, sleeping, termination, etc. The scheduler algorithm is configurable
by cargo features.

## Features

- Multi-task scheduling, IRQ handling, and timer-based APIs such as `sleep`,
  `sleep_until`, and `WaitQueue::wait_timeout` are always available.
- `preempt`: Enable preemptive scheduling.
- FIFO cooperative scheduler is the default when no scheduler feature is selected.
- `sched-rr`: Use the Round-robin preemptive scheduler (enables `preempt`).
- `sched-cfs`: Use the Completely Fair Scheduler (enables `preempt`).
- `tls`: Enable kernel space thread-local storage support.
- `smp`: Enable SMP (symmetric multiprocessing) support.

## License

This project is licensed under GPL-3.0-or-later OR Apache-2.0 OR MulanPSL-2.0.
