//! Top-level feature selection for [ArceOS].
//!
//! # Cargo Features
//!
//! - CPU
//!     - `smp`: Enable SMP (symmetric multiprocessing) support.
//!     - `fp-simd`: Enable floating point and SIMD support.
//! - Interrupts:
//!     - `irq`: Enable interrupt handling support.
//!     - `ipi`: Enable Inter-Processor Interrupts (IPIs).
//! - Memory
//!     - `alloc`: Enable dynamic memory allocation.
//!     - `paging`: Enable page table manipulation.
//!     - `tls`: Enable thread-local storage.
//! - Task management
//!     - `multitask`: Enable multi-threading support.
//!     - `sched-fifo`: Use the FIFO cooperative scheduler.
//!     - `sched-rr`: Use the Round-robin preemptive scheduler.
//!     - `sched-cfs`: Use the Completely Fair Scheduler (CFS) preemptive scheduler.
//!     - `stack-protector`: Enable compiler-inserted stack frame canary checks.
//! - Upperlayer stacks (fs, net, display)
//!     - `fs`: Enable file system support.
//!     - `net`: Enable networking support.
//!     - `display`: Enable graphics support.
//! - Device drivers are selected directly through `ax-driver/*` features by
//!   board configurations.
//!
//! [ArceOS]: https://github.com/arceos-org/arceos

#![no_std]
