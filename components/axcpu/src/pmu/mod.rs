//! Hardware performance monitoring for the current CPU.

pub use crate::arch::current::pmu::{CounterId, EventConfig, EventSupport, Pmu, PmuError, PmuInfo};
