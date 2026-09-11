pub mod config;
mod request;

pub(crate) const VIRTIO_BLK_REQUEST_HEADER_SIZE: u32 = 16;

pub use request::{BlockQueueOutcome, VirtioBlockRequestCore};
