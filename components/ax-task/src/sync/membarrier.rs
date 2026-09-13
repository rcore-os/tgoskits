//! Scheduler-mediated memory barriers and address-space registration.

pub use crate::{
    sync::membarrier::operations::{
        MembarrierCommand, refresh_current_membarrier_run_queue, register_current_membarrier,
    },
    thread::error::MembarrierError,
};

pub(crate) mod operations;

pub use self::operations::membarrier;
