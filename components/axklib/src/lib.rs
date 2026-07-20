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

//! axklib — small kernel-helper abstractions used across the microkernel
//!
//! This crate exposes a tiny, no_std-compatible trait (`Klib`) that the
//! platform/board layer must implement. The trait provides a handful of
//! common kernel helpers such as memory mapping helpers, timing utilities,
//! and IRQ registration. The implementation is supplied by the platform
//! (see `modules/axklib-impl`) and consumed by drivers and other modules.
//!
//! The crate also provides small convenience modules (`mem`, `time`, `irq`)
//! that re-export the trait methods with shorter names to make call sites
//! more ergonomic.
//!
//! Example usage:
//!
//! ```ignore
//! // map 4K of device MMIO at physical address `paddr`
//! let vaddr = axklib::mem::iomap(paddr, 0x1000)?;
//!
//! // busy-wait for 100 microseconds
//! axklib::time::busy_wait(core::time::Duration::from_micros(100));
//!
//! // request a shared IRQ action
//! let irq = axklib::irq::try_legacy_irq(32)?;
//! let handle = axklib::irq::request_shared(irq, my_irq_handler)?;
//! ```

#![no_std]
// #![allow(missing_docs)]

extern crate alloc;

use core::time::Duration;

pub use ax_errno::{AxError, AxResult};
pub use ax_memory_addr::{PhysAddr, VirtAddr};
pub use irq_framework::{
    AutoEnable as IrqAutoEnable, BoxedIrqHandler, ConcurrentBoxedIrqHandler, CpuId as IrqCpuId,
    CpuMask as IrqCpuMask, IrqAffinity, IrqContext, IrqError, IrqExecution, IrqHandle, IrqId,
    IrqOutcome, IrqRequest, IrqReturn, IrqScope, IrqStatus, ShareMode as IrqShareMode,
};
use trait_ffi::*;

/// Compatibility IRQ domain used while non-domainized callers migrate.
pub const LEGACY_IRQ_DOMAIN: irq_framework::IrqDomainId = irq_framework::IrqDomainId(0);

/// Creates a legacy IRQ id without truncating the raw IRQ number.
pub fn try_legacy_irq(raw: usize) -> Result<IrqId, IrqError> {
    let hwirq = u32::try_from(raw).map_err(|_| IrqError::InvalidIrq)?;
    Ok(IrqId::new(LEGACY_IRQ_DOMAIN, irq_framework::HwIrq(hwirq)))
}

/// Compatibility constructor for legacy numeric IRQ users.
pub fn legacy_irq(raw: usize) -> Result<IrqId, IrqError> {
    try_legacy_irq(raw)
}

/// Returns the legacy raw IRQ number when this id is in the legacy domain.
pub const fn legacy_irq_raw(irq: IrqId) -> Option<usize> {
    if irq.domain.0 == LEGACY_IRQ_DOMAIN.0 {
        Some(irq.hwirq.0 as usize)
    } else {
        None
    }
}

/// Legacy constructor kept only for upper-layer compatibility.
#[allow(non_snake_case)]
pub fn IrqNumber(raw: usize) -> Result<IrqId, IrqError> {
    legacy_irq(raw)
}

pub mod dma;
pub mod mmio;

/// The kernel helper trait that platform implementations must provide.
#[def_extern_trait]
pub trait Klib {
    /// Map a physical memory region into the kernel's virtual address space.
    ///
    /// Parameters:
    /// - `addr`: The physical start address of the region to map.
    /// - `size`: The size in bytes of the region to map. Typically page-aligned.
    ///
    /// Returns:
    /// - `Ok(VirtAddr)` with the virtual address corresponding to the mapped
    ///   physical region on success.
    /// - `Err(_)` with an `AxResult` error code on failure.
    ///
    /// Notes:
    /// - The returned `VirtAddr` is page-aligned when the underlying mapping
    ///   mechanism requires it.
    /// - The actual mapping behavior is platform-specific; callers should
    ///   treat this as an allocation-like operation and ensure the mapping
    ///   is later cleaned up if the platform/ABI requires it.
    fn mem_iomap(addr: PhysAddr, size: usize) -> AxResult<VirtAddr>;

    /// Translates a kernel virtual address to the corresponding physical address.
    fn mem_virt_to_phys(addr: VirtAddr) -> PhysAddr;

    /// Converts newly allocated DMA-coherent pages to an uncached kernel mapping.
    ///
    /// This is not a general-purpose memory attribute switching API. Callers
    /// must only use it for pages that were just allocated for
    /// `alloc_coherent`, are page-owned by that allocation, and have not been
    /// exposed to another CPU, mapping, or device.
    ///
    /// Implementations must perform the required cache maintenance, TLB
    /// invalidation, and ordering barriers internally.
    fn mem_make_dma_coherent_uncached(addr: VirtAddr, size: usize) -> AxResult;

    /// Restores DMA-coherent pages to a normal cacheable kernel mapping.
    ///
    /// The caller must ensure the device no longer owns or accesses the pages.
    /// Implementations must perform the required TLB invalidation and ordering
    /// barriers internally before the pages are returned to the normal page
    /// allocator.
    fn mem_restore_dma_cached(addr: VirtAddr, size: usize) -> AxResult;

    /// Cleans a CPU cache range before device ownership.
    fn dma_cache_clean(_addr: VirtAddr, _size: usize) {}

    /// Invalidates a CPU cache range after device writes.
    fn dma_cache_invalidate(_addr: VirtAddr, _size: usize) {}

    /// Cleans and invalidates a CPU cache range for bidirectional DMA.
    fn dma_cache_clean_invalidate(_addr: VirtAddr, _size: usize) {}

    /// Allocates contiguous DMA pages.
    ///
    /// `dma_mask` is the device-visible address mask. Implementations should
    /// use a DMA32-capable allocator when the mask requires it.
    fn dma_alloc_pages(dma_mask: u64, num_pages: usize, align: usize) -> AxResult<VirtAddr>;

    /// Releases pages previously allocated by [`Klib::dma_alloc_pages`].
    fn dma_dealloc_pages(addr: VirtAddr, num_pages: usize);

    /// Busy-wait the current execution context for the provided duration.
    ///
    /// This is intended for short delays where sleeping or timer-based
    /// suspension is not available or not appropriate (for example, very
    /// early boot or simple spin-waits). Implementations should aim to be
    /// reasonably accurate for small durations but exact timing guarantees
    /// are platform-dependent.
    fn time_busy_wait(dur: Duration);

    /// Returns monotonic time in nanoseconds.
    fn time_monotonic_nanos() -> u64;

    /// Initializes the wall-clock epoch offset from an absolute epoch time.
    fn time_try_init_epoch_offset(epoch_time_nanos: u64) -> bool;

    /// Request a shared IRQ action and return its handle on success.
    fn irq_request_shared(irq: IrqId, handler: BoxedIrqHandler) -> AxResult<IrqHandle>;

    /// Request a shared IRQ action without enabling it.
    fn irq_request_shared_disabled(irq: IrqId, handler: BoxedIrqHandler) -> AxResult<IrqHandle>;

    /// Request a per-CPU IRQ action and return its handle on success.
    fn irq_request_percpu(
        irq: IrqId,
        cpus: IrqCpuMask,
        handler: ConcurrentBoxedIrqHandler,
    ) -> AxResult<IrqHandle>;

    /// Free an IRQ action previously returned by a request function.
    fn irq_free(handle: IrqHandle) -> AxResult;

    /// Enable an IRQ action by handle.
    fn irq_enable(handle: IrqHandle) -> AxResult;

    /// Disable an IRQ action by handle.
    fn irq_disable(handle: IrqHandle) -> AxResult;

    /// Runs a raw thunk synchronously on the requested CPU.
    ///
    /// This is an owner-context bridge for driver runtimes that must keep all
    /// register access on a fixed CPU. Platform glue should override this when
    /// cross-CPU IPI execution is available.
    ///
    /// # Safety
    ///
    /// `arg` must stay valid until the function returns, and `f` must be safe
    /// to execute in the target CPU's IRQ/IPI context.
    unsafe fn irq_run_on_cpu_sync(
        cpu: IrqCpuId,
        f: unsafe fn(*mut ()),
        arg: *mut (),
    ) -> Result<(), IrqError> {
        if cpu.0 == 0 {
            unsafe { f(arg) };
            Ok(())
        } else {
            Err(IrqError::Unsupported)
        }
    }
}

/// Convenience re-export for memory IO mapping.
pub mod mem {
    pub use super::klib::{
        mem_iomap as iomap, mem_make_dma_coherent_uncached as make_dma_coherent_uncached,
        mem_restore_dma_cached as restore_dma_cached, mem_virt_to_phys as virt_to_phys,
    };
}

/// Convenience re-export for busy-wait timing.
pub mod time {
    pub use super::klib::{
        time_busy_wait as busy_wait, time_monotonic_nanos as monotonic_nanos,
        time_try_init_epoch_offset as try_init_epoch_offset,
    };
}

/// Convenience re-exports for IRQ operations.
pub mod irq {
    pub use super::{
        BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqAffinity, IrqAutoEnable as AutoEnable,
        IrqContext, IrqCpuId as CpuId, IrqCpuMask as CpuMask, IrqError, IrqExecution, IrqHandle,
        IrqId, IrqNumber, IrqOutcome, IrqRequest, IrqReturn, IrqScope, IrqShareMode as ShareMode,
        IrqStatus,
        klib::{
            irq_disable as disable, irq_enable as enable, irq_free as free,
            irq_run_on_cpu_sync as run_on_cpu_sync,
        },
        legacy_irq, legacy_irq_raw, try_legacy_irq,
    };

    /// Request a shared IRQ action and return its handle on success.
    pub fn request_shared(
        irq: IrqId,
        handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
    ) -> super::AxResult<IrqHandle> {
        super::klib::irq_request_shared(irq, alloc::boxed::Box::new(handler))
    }

    /// Request a shared IRQ action without enabling it.
    pub fn request_shared_disabled(
        irq: IrqId,
        handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
    ) -> super::AxResult<IrqHandle> {
        super::klib::irq_request_shared_disabled(irq, alloc::boxed::Box::new(handler))
    }

    /// Request a per-CPU IRQ action and return its handle on success.
    pub fn request_percpu(
        irq: IrqId,
        cpus: CpuMask,
        handler: impl Fn(IrqContext) -> IrqReturn + Send + Sync + 'static,
    ) -> super::AxResult<IrqHandle> {
        super::klib::irq_request_percpu(irq, cpus, alloc::boxed::Box::new(handler))
    }
}
