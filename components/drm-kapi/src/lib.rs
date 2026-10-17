// SPDX-License-Identifier: MIT
//! Portable Linux-kernel API mechanisms consumed by the shared DRM core.
//!
//! This crate contains DRM-relevant Linux semantics and algorithms only. It
//! has no operating-system dependency: memory, task scheduling, current-user
//! address-space access, file descriptors, and device services cross narrow
//! provider interfaces. The ArceOS and Starry integrations install concrete
//! providers from their own runtime layers.
#![no_std]
extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod assert;
pub mod bitmap;
pub mod bits;
pub mod clock;
pub mod completion;
pub mod config;
pub mod dma;
pub mod dma_fence;
pub mod dma_fence_array;
pub mod dma_fence_chain;
pub mod dma_fence_unwrap;
pub mod dma_resv;
pub mod drm_mm;
pub mod fields;
pub mod gem_memory;
pub mod i2c;
pub mod idr;
pub mod iopoll;
pub mod irq_work;
pub mod kernel_core;
pub mod kernel_services;
pub mod kthread;
pub mod list;
pub mod locks;
pub mod memory;
pub mod mm;
pub mod mm_native;
pub mod mmu_notifier;
pub mod mmu_notifier_domain;
pub mod mutex;
pub mod page_cache;
pub mod primitives;
pub mod print;
pub mod rbtree;
pub mod rcu;
pub mod scatterlist;
pub mod seq_file;
pub mod shmem;
pub mod signal;
pub mod sync_file;
pub mod task_runtime;
pub mod timer;
pub mod types;
pub mod vm;
pub mod wait;
pub mod workqueue;
pub mod ww_mutex;
pub mod xarray;
