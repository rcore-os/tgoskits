#![no_std]

//! # ARM GIC Driver
//!
//! A driver for the ARM Generic Interrupt Controller (GIC).
//!
//! ## Platform Support
//!
//! This driver is designed for ARM AArch64 systems and provides:
//!
//! - **GICv2 support**: Available on both 32-bit and 64-bit ARM platforms
//! - **GICv3 support**: Only available on 64-bit ARM (AArch64) platforms
//! - **System Register access**: Only available on AArch64 platforms
//!
//! ### Platform-Specific Modules
//!
//! - The [`v3`] module is **only available on AArch64** (`target_arch = "aarch64"`)
//!
//! If you're working on a non-ARM platform, most of this driver's functionality
//! will not be available at compile time.

pub(crate) mod define;
pub mod sys_reg;

#[cfg(test)]
mod tests;
mod version;

use core::{
    fmt::{Debug, Display},
    ptr::NonNull,
};

pub use define::IntId;
pub use version::*;

/// Virtual address wrapper for memory-mapped register access.
///
/// This type provides a safe wrapper around virtual addresses used for accessing
/// memory-mapped registers in the GIC. It ensures type safety while allowing
/// efficient pointer operations.
///
/// # Examples
///
/// ```no_run
/// use arm_gic_driver::VirtAddr;
///
/// let addr = VirtAddr::new(0xF900_0000);
/// let ptr: *mut u32 = addr.as_ptr();
/// ```
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct VirtAddr(usize);

impl VirtAddr {
    /// Create a new `VirtAddr` from a raw address value.
    ///
    /// # Arguments
    ///
    /// * `val` - The virtual address as a usize value
    ///
    /// # Examples
    ///
    /// ```
    /// use arm_gic_driver::VirtAddr;
    ///
    /// let addr = VirtAddr::new(0xF900_0000);
    /// ```
    pub const fn new(val: usize) -> Self {
        Self(val)
    }

    /// Get the virtual address as a raw pointer of the specified type.
    ///
    /// # Type Parameters
    ///
    /// * `T` - The target pointer type
    ///
    /// # Returns
    ///
    /// A raw mutable pointer to type `T`
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - The address is valid for the target type `T`
    /// - The memory region is properly mapped and accessible
    /// - Appropriate synchronization is used for concurrent access
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use arm_gic_driver::VirtAddr;
    ///
    /// let addr = VirtAddr::new(0xF900_0000);
    /// let ptr: *mut u32 = addr.as_ptr();
    /// ```
    pub const fn as_ptr<T>(&self) -> *mut T {
        self.0 as *mut T
    }
}

impl From<usize> for VirtAddr {
    fn from(addr: usize) -> Self {
        Self(addr)
    }
}

impl From<VirtAddr> for usize {
    fn from(addr: VirtAddr) -> Self {
        addr.0
    }
}

impl From<*mut u8> for VirtAddr {
    fn from(addr: *mut u8) -> Self {
        Self(addr as usize)
    }
}

impl<T> From<NonNull<T>> for VirtAddr {
    fn from(addr: NonNull<T>) -> Self {
        Self(addr.as_ptr() as usize)
    }
}

impl Display for VirtAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "VirtAddr({:#p})", self.0 as *const u8)
    }
}
