# axsched

[![Crates.io](https://img.shields.io/crates/v/axsched)](https://crates.io/crates/axsched)
[![Docs.rs](https://docs.rs/axsched/badge.svg)](https://docs.rs/axsched)
[![CI](https://github.com/arceos-org/axsched/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/axsched/actions/workflows/ci.yml)

Various scheduler algorithms in a unified interface.

Currently supported algorithms:

- [`FifoScheduler`]: FIFO (First-In-First-Out) scheduler (cooperative).
- [`RRScheduler`]: Round-robin scheduler (preemptive).
- [`CFScheduler`]: Completely Fair Scheduler (preemptive).

[`FifoScheduler`]: https://docs.rs/axsched/latest/axcpu/struct.FifoScheduler.html
[`RRScheduler`]: https://docs.rs/axsched/latest/axcpu/struct.RRScheduler.html
[`CFScheduler`]: https://docs.rs/axsched/latest/axcpu/struct.CFS

## Example

```rust
use std::sync::Arc;
use axsched::{FifoScheduler, FifoTask, BaseScheduler};

let mut scheduler = FifoScheduler::new();
scheduler.init();

for i in 0..10 {
    let task = FifoTask::new(i);
    scheduler.add_task(Arc::new(task));
}

for i in 0..10 {
    let next = scheduler.pick_next_task().unwrap();
    let task_id = *next.inner();
    println!("Task {task_id} is running...");
    assert_eq!(task_id, i);
    scheduler.put_prev_task(next, false);
}
```
