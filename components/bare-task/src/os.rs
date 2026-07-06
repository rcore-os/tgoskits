//! Convenience wrappers around the `BareTaskOs` trait-ffi ABI.

/// CPU convenience wrappers.
pub mod cpu {
    pub use crate::bare_task_os::{cpu_num, current_task_ptr, set_current_task_ptr, this_cpu_id};
}

/// IRQ convenience wrappers.
pub mod irq {
    pub use crate::bare_task_os::{
        in_irq_context, irq_restore, irq_save_and_disable, irqs_enabled, wait_for_irqs,
    };
}

/// Time convenience wrappers.
pub mod time {
    pub use crate::bare_task_os::{monotonic_time_nanos, set_oneshot_timer};
}

/// SMP convenience wrappers.
pub mod smp {
    pub use crate::bare_task_os::{request_irq_wake, request_reschedule, wait_until_cpu_ready};
}
