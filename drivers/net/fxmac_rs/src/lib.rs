//! # FXMAC Ethernet Driver
//!
//! A `no_std` Rust driver for the FXMAC Ethernet controller found on the PhytiumPi (Phytium Pi) board.
//! This driver supports DMA-based packet transmission and reception, providing a foundation for
//! network communication in embedded and bare-metal environments.
//!
//! ## Features
//!
//! - **DMA Support**: Efficient packet transmission and reception using DMA buffer descriptors.
//! - **PHY Management**: Support for PHY initialization, auto-negotiation, and manual speed configuration.
//! - **Interrupt Handling**: Built-in interrupt handlers for TX/RX completion and error conditions.
//! - **Multiple PHY Interfaces**: Support for SGMII, RGMII, RMII, XGMII, and other interface modes.
//! - **Configurable**: Supports jumbo frames, multicast filtering, and various MAC options.
//!
//! ## Target Platform
//!
//! This driver is designed for the aarch64 architecture, specifically targeting the PhytiumPi board
//! with the Motorcomm YT8521 PHY.
//!
//! ## Quick Start
//!
//! To use this driver, you need to implement the [`KernelFunc`] trait to provide the necessary
//! kernel functions for DMA address translation and coherent allocation. The
//! OS maps the controller once and passes that mapping to `discover_xmac`.
//!
//! ```ignore
//! use fxmac_rs::{KernelFunc, begin_xmac_init, poll_xmac_init, FXmacLwipPortTx, FXmacRecvHandler};
//!
//! // Implement the KernelFunc trait for your platform
//! pub struct FXmacDriver;
//!
//! #[ax_crate_interface::impl_interface]
//! impl KernelFunc for FXmacDriver {
//!     fn virt_to_phys(addr: usize) -> usize {
//!         // Your implementation
//!         addr
//!     }
//!
//!     fn dma_alloc_coherent(pages: usize) -> (usize, usize) {
//!         // Your implementation: returns (virtual_addr, physical_addr)
//!         unimplemented!()
//!     }
//!
//!     fn dma_free_coherent(vaddr: usize, pages: usize) {
//!         // Your implementation
//!     }
//!
//! }
//!
//! // Initialize the driver
//! let hwaddr: [u8; 6] = [0x55, 0x44, 0x33, 0x22, 0x11, 0x00];
//! let discovery = unsafe { discover_xmac(mapped_base, mapped_size)? };
//! let (pending, irq_port) = discovery.into_parts();
//! let mut initialization = begin_xmac_init(pending);
//! let fxmac = loop {
//!     match poll_xmac_init(&mut initialization, monotonic_now_ns()) {
//!         FXmacInitPoll::Ready => break initialization.take_ready()?,
//!         FXmacInitPoll::Pending(schedule) => schedule_next(schedule),
//!         FXmacInitPoll::Failed(error) => return Err(error),
//!     }
//! };
//!
//! // Send packets
//! let mut tx_vec = Vec::new();
//! tx_vec.push(packet_data.to_vec());
//! FXmacLwipPortTx(fxmac, tx_vec);
//!
//! // Receive packets
//! if let Some(recv_packets) = FXmacRecvHandler(fxmac) {
//!     for packet in recv_packets {
//!         // Process received packet
//!     }
//! }
//! ```
//!
//! ## Module Structure
//!
//! - [`fxmac`]: Core MAC controller functionality and configuration.
//! - [`fxmac_dma`]: DMA buffer descriptor management and packet handling.
//! - [`fxmac_intr`]: Interrupt handling and callback management.
//! - [`fxmac_phy`]: PHY initialization and management functions.
//!
//! ## Safety and Environment
//!
//! - This crate targets `no_std` and assumes the platform provides DMA-coherent
//!   memory and interrupt routing.
//! - Most APIs interact with memory-mapped registers and should be used with
//!   care in the correct execution context.
//!
//! ## Feature Flags
//!
//! - `debug`: Enable logging via the `log` crate. Without this feature, logging
//!   macros become no-ops.

#![no_std]
#![allow(unused)]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

extern crate alloc;

#[cfg(feature = "debug")]
#[macro_use]
extern crate log;

#[cfg(not(feature = "debug"))]
#[macro_use]
mod log {
    macro_rules! trace {
        ($($arg:tt)*) => {};
    }
    macro_rules! debug {
        ($($arg:tt)*) => {};
    }
    macro_rules! info {
        ($($arg:tt)*) => {};
    }
    macro_rules! warn {
        ($($arg:tt)*) => {};
    }
    macro_rules! error {
        ($($arg:tt)*) => {};
    }
}

// mod mii_const;
mod fxmac_const;

mod fxmac;
mod fxmac_dma;
mod fxmac_intr;
mod fxmac_phy;
mod utils;

// Re-exports for core MAC functionality
pub use fxmac::*;
// Re-exports for DMA operations
pub use fxmac_dma::*;
// Re-exports for interrupt handling
// Re-exports for PHY interface
pub use fxmac_phy::{FXmacPhyInit, FXmacPhyRead, FXmacPhyWrite};

/// Kernel function interface required by the FXMAC Ethernet driver.
///
/// This trait defines the platform-specific functions that must be implemented
/// by the host system to support the FXMAC driver. These functions handle
/// DMA address translation and coherent memory management.
///
/// # Implementation Requirements
///
/// All implementations must be `#[ax_crate_interface::impl_interface]` compatible
/// and provide thread-safe operations where applicable.
///
/// # Example
///
/// ```ignore
/// pub struct MyPlatform;
///
/// #[ax_crate_interface::impl_interface]
/// impl fxmac_rs::KernelFunc for MyPlatform {
///     fn virt_to_phys(addr: usize) -> usize {
///         // Platform-specific virtual to physical address translation
///         addr - KERNEL_OFFSET
///     }
///
///     fn dma_alloc_coherent(pages: usize) -> (usize, usize) {
///         // Allocate DMA-capable coherent memory
///         // Returns (virtual_address, physical_address)
///         allocator.alloc_dma(pages)
///     }
///
///     fn dma_free_coherent(vaddr: usize, pages: usize) {
///         // Free previously allocated DMA memory
///         allocator.free_dma(vaddr, pages)
///     }
///
/// }
/// ```
#[ax_crate_interface::def_interface]
pub trait KernelFunc {
    /// Converts a virtual address to its corresponding physical address.
    ///
    /// This function is used by the driver to obtain physical addresses for
    /// DMA operations, as the hardware requires physical addresses for
    /// buffer descriptors.
    ///
    /// # Arguments
    ///
    /// * `addr` - The virtual address to convert.
    ///
    /// # Returns
    ///
    /// The corresponding physical address.
    fn virt_to_phys(addr: usize) -> usize;

    /// Allocates DMA-coherent memory pages.
    ///
    /// Allocates physically contiguous memory that is suitable for DMA
    /// operations. The memory should be cache-coherent or properly managed
    /// for DMA access.
    ///
    /// # Arguments
    ///
    /// * `pages` - The number of pages (typically 4KB each) to allocate.
    ///
    /// # Returns
    ///
    /// A tuple containing `(virtual_address, physical_address)` of the
    /// allocated memory region.
    fn dma_alloc_coherent(pages: usize) -> (usize, usize);

    /// Frees previously allocated DMA-coherent memory.
    ///
    /// # Arguments
    ///
    /// * `vaddr` - The virtual address of the memory region to free.
    /// * `pages` - The number of pages to free.
    fn dma_free_coherent(vaddr: usize, pages: usize);
}

#[cfg(test)]
mod tests {
    use core::alloc::Layout;

    struct TestKernelFunc;

    #[ax_crate_interface::impl_interface]
    impl super::KernelFunc for TestKernelFunc {
        fn virt_to_phys(addr: usize) -> usize {
            addr
        }

        fn dma_alloc_coherent(pages: usize) -> (usize, usize) {
            let layout = Layout::from_size_align(pages * 4096, 4096).unwrap();
            let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) } as usize;
            assert_ne!(ptr, 0);
            (ptr, ptr)
        }

        fn dma_free_coherent(vaddr: usize, pages: usize) {
            let layout = Layout::from_size_align(pages * 4096, 4096).unwrap();
            unsafe { alloc::alloc::dealloc(vaddr as *mut u8, layout) };
        }
    }

    #[test]
    fn it_works() {}
}
