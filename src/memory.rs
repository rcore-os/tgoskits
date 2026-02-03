// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Memory allocation and address translation APIs for the AxVisor hypervisor.
//!
//! This module provides APIs for physical memory management, including frame
//! allocation/deallocation and physical-virtual address translation.
//!
//! # Overview
//!
//! The memory APIs are fundamental to the hypervisor's operation, enabling:
//! - Physical frame allocation for guest memory
//! - Contiguous frame allocation for DMA and other hardware requirements
//! - Address translation between physical and virtual addresses
//!
//! # Re-exports
//!
//! This module re-exports [`PhysAddr`] and [`VirtAddr`] from the `memory_addr`
//! crate for convenience.
//!
//! # Types
//!
//! - [`PhysFrame`] - A physical frame that is automatically deallocated when
//!   dropped.
//!
//! # Implementation
//!
//! To implement these APIs, use the [`api_impl`](crate::api_impl) attribute
//! macro on an impl block:
//!
//! ```rust,ignore
//! struct MemoryIfImpl;
//!
//! #[axvisor_api::api_impl]
//! impl axvisor_api::memory::MemoryIf for MemoryIfImpl {
//!     fn alloc_frame() -> Option<PhysAddr> {
//!         // Allocate a physical frame from your allocator
//!     }
//!     // ... implement other functions
//! }
//! ```

pub use memory_addr::{PhysAddr, VirtAddr};

/// The API trait for memory allocation and address translation functionalities.
///
/// This trait defines the core memory management interface required by the
/// hypervisor. Implementations should be provided by the host system or HAL
/// layer.
#[crate::api_def]
pub trait MemoryIf {
    /// Allocate a single physical frame (4KB page).
    ///
    /// # Returns
    ///
    /// - `Some(PhysAddr)` - The physical address of the allocated frame.
    /// - `None` - If allocation fails (e.g., out of memory).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use axvisor_api::memory::{alloc_frame, dealloc_frame};
    ///
    /// if let Some(frame) = alloc_frame() {
    ///     // Use the frame...
    ///     dealloc_frame(frame);
    /// }
    /// ```
    fn alloc_frame() -> Option<PhysAddr>;

    /// Allocate a number of contiguous physical frames with a specified
    /// alignment.
    ///
    /// This function is useful for allocating memory for DMA buffers or other
    /// hardware that requires contiguous physical memory.
    ///
    /// # Arguments
    ///
    /// * `num_frames` - The number of contiguous frames to allocate.
    /// * `frame_align_pow2` - The alignment requirement as a power of 2
    ///   (e.g., 0 for 4KB alignment, 1 for 8KB alignment).
    ///
    /// # Returns
    ///
    /// - `Some(PhysAddr)` - The physical address of the first allocated frame.
    /// - `None` - If allocation fails.
    fn alloc_contiguous_frames(num_frames: usize, frame_align_pow2: usize) -> Option<PhysAddr>;

    /// Deallocate a frame previously allocated by [`alloc_frame`].
    ///
    /// # Arguments
    ///
    /// * `addr` - The physical address of the frame to deallocate.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - The address was previously returned by [`alloc_frame`].
    /// - The frame has not been deallocated yet.
    /// - No references to the frame's memory exist after deallocation.
    fn dealloc_frame(addr: PhysAddr);

    /// Deallocate contiguous frames previously allocated by
    /// [`alloc_contiguous_frames`].
    ///
    /// # Arguments
    ///
    /// * `first_addr` - The physical address of the first frame.
    /// * `num_frames` - The number of frames to deallocate.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - The address and count match a previous [`alloc_contiguous_frames`]
    ///   call.
    /// - The frames have not been deallocated yet.
    /// - No references to the frames' memory exist after deallocation.
    fn dealloc_contiguous_frames(first_addr: PhysAddr, num_frames: usize);

    /// Convert a physical address to a virtual address.
    ///
    /// This function performs the physical-to-virtual address translation
    /// based on the host's memory mapping.
    ///
    /// # Arguments
    ///
    /// * `addr` - The physical address to convert.
    ///
    /// # Returns
    ///
    /// The corresponding virtual address.
    ///
    /// # Panics
    ///
    /// May panic if the physical address is not mapped.
    fn phys_to_virt(addr: PhysAddr) -> VirtAddr;

    /// Convert a virtual address to a physical address.
    ///
    /// This function performs the virtual-to-physical address translation
    /// based on the host's memory mapping.
    ///
    /// # Arguments
    ///
    /// * `addr` - The virtual address to convert.
    ///
    /// # Returns
    ///
    /// The corresponding physical address.
    ///
    /// # Panics
    ///
    /// May panic if the virtual address is not mapped.
    fn virt_to_phys(addr: VirtAddr) -> PhysAddr;
}

/// [`AxMmHal`](axaddrspace::AxMmHal) implementation by axvisor_api.
///
/// This struct provides an implementation of the `AxMmHal` trait from the
/// `axaddrspace` crate, delegating to the axvisor_api memory functions.
#[doc(hidden)]
pub struct AxMmHalApiImpl;

impl axaddrspace::AxMmHal for AxMmHalApiImpl {
    fn alloc_frame() -> Option<PhysAddr> {
        alloc_frame()
    }

    fn dealloc_frame(addr: PhysAddr) {
        dealloc_frame(addr)
    }

    fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
        phys_to_virt(addr)
    }

    fn virt_to_phys(addr: VirtAddr) -> PhysAddr {
        virt_to_phys(addr)
    }
}

/// A physical frame which will be automatically deallocated when dropped.
///
/// This type alias provides a convenient RAII wrapper around physical frame
/// allocation. When a `PhysFrame` is dropped, it automatically deallocates
/// the underlying physical memory.
///
/// # Example
///
/// ```rust,ignore
/// use axvisor_api::memory::PhysFrame;
///
/// fn allocate_guest_memory() -> Option<PhysFrame> {
///     PhysFrame::alloc()
/// }
///
/// // The frame will be automatically deallocated when it goes out of scope
/// ```
pub type PhysFrame = axaddrspace::PhysFrame<AxMmHalApiImpl>;
