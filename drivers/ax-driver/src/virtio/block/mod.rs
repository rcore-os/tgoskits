//! VirtIO block discovery, initialization, IRQ, and RDIF domain boundary.
//!
//! Normal I/O consumes the used ring only from an acknowledged queue IRQ or
//! its explicit deferred-ack continuation. Transport contention is coalesced
//! without fabricating completion evidence. Requests whose used descriptor
//! cannot be consumed remain device-owned until controller quiescence, and an
//! unexpected live Drop quarantines the queue with all descriptor storage.

mod controller;
mod device;
mod discovery;
mod initialization;
mod irq;
mod lifecycle;
mod queue;

pub use discovery::register_transport;

pub(super) const VIRTIO_BLK_QUEUE_ID: usize = 0;
pub(super) const VIRTIO_BLK_IRQ_SOURCE_ID: usize = 0;

#[cfg(test)]
mod tests;
