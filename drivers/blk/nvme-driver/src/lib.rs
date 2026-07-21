#![no_std]

extern crate alloc;

mod block;
mod command;
pub mod err;
mod initialization;
mod lifecycle;
mod nvme;
mod queue;
mod registers;

use core::{alloc::Layout, ptr::NonNull};

pub use block::{NvmeBlockActivator, NvmeBlockDriver};
pub use nvme::{Config, Namespace};

#[derive(Clone, Copy)]
pub struct DMAMem {
    pub virt: NonNull<u8>,
    pub phys: u64,
    pub layout: Layout,
}
