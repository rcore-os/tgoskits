#![no_std]

//! Linux-compatible virtual-memory policy independent of syscall and VFS glue.

extern crate alloc;

mod accounting;
mod capability;
mod cow;
mod fault;
mod pages;
mod policy;
mod stats;
mod vm_stat;

pub use self::{
    accounting::*, capability::*, cow::*, fault::*, pages::*, policy::*, stats::*,
    vm_stat::ProcessVmStat,
};
