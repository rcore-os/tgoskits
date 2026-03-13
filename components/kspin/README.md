# kspin

[![Crates.io](https://img.shields.io/crates/v/kspin)](https://crates.io/crates/kspin)
[![Docs.rs](https://docs.rs/kspin/badge.svg)](https://docs.rs/kspin)
[![CI](https://github.com/arceos-org/kspin/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/kspin/actions/workflows/ci.yml)

Spinlocks used for kernel space that can disable preemption or IRQs in the
critical section.

## Cargo Features

- `smp`: Use in the **multi-core** environment. For **single-core** environment (without this feature), the lock state is unnecessary and optimized out. CPU can always get the lock if we follow the proper guard in use. By default, this feature is disabled.

## Examples

```rust
use kspin::{SpinNoIrq, SpinNoPreempt, SpinRaw};

let data = SpinRaw::new(());
let mut guard = data.lock();
/* critical section, does nothing while trying to lock. */
drop(guard);

let data = SpinNoPreempt::new(());
let mut guard = data.lock();
/* critical section, preemption are disabled. */
drop(guard);

let data = SpinNoIrq::new(());
let mut guard = data.lock();
/* critical section, both preemption and IRQs are disabled. */
drop(guard);
```


