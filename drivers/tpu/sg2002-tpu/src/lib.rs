//! SG2002 (Cvitek) TPU and Ion memory allocator driver layer.
//!
//! This crate provides a hardware-only, OS-glue-free abstraction for the
//! Cvitek SG2002 TPU together with the Ion buffer manager that the TPU relies
//! on for DMA buffers. The high-level OS bindings (file descriptors, ioctl
//! plumbing, mmap) live in the consuming kernel and call into the types
//! exported here.

#![no_std]

extern crate alloc;
#[macro_use]
extern crate log;

pub mod ion;
pub mod tpu;
