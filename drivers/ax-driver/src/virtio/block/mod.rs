//! VirtIO block discovery, initialization, IRQ, and RDIF domain boundary.
//!
//! Normal I/O consumes the used ring only from an acknowledged queue IRQ.
//! Destructive interrupt status belongs to an independent port moved into the
//! registered callback, while the CPU-pinned maintenance owner alone advances
//! transport and queue state. Requests whose used descriptor cannot be
//! consumed remain device-owned until controller quiescence.

#[cfg(test)]
mod controller;
mod device;
mod discovery;
mod initialization;
mod irq;
#[cfg(test)]
mod lifecycle;
mod notify;
mod queue;
mod v13;

pub use discovery::{
    register_mmio_transport, register_transport, register_transport_with_interrupt_port,
};
pub use irq::VirtioInterruptPort;
pub use notify::VirtioQueueNotifyPort;
pub use v13::VirtioBlockActivator;

pub(super) const VIRTIO_BLK_QUEUE_ID: usize = 0;
pub(super) const VIRTIO_BLK_IRQ_SOURCE_ID: usize = 0;

#[cfg(test)]
mod tests;
