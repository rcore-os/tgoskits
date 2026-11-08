//! Portable physical block runtime, coherent cache, and volume layout.
#![cfg_attr(not(any(test, feature = "host-test")), no_std)]
extern crate alloc;
#[cfg(test)]
extern crate ax_runtime;
#[macro_use]
extern crate log;
pub mod access;
pub mod block;
mod error;
pub mod os;
pub mod volume;
pub use access::{DiskMaintenance, VolumeAccess, VolumeDescriptor, VolumeLease, VolumePermissions};
#[cfg(feature = "cache")]
pub use block::RegionBlockDevice;
pub use block::{
    BlockDevice, BlockRegion,
    runtime::{BlockIoStats, DeviceRef},
};
pub use error::{BlockError, BlockResult};
