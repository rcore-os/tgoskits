//! Platform implementation of the `axklib::Klib` trait.
//!
//! This crate provides the platform-side glue that implements the small set
//! of kernel helper functions defined in `axklib`. The implementation is
//! intentionally minimal: it forwards memory mapping requests to `axmm`,
//! delegates timing to `ax-hal`, and wires IRQ operations to `ax-hal` when the
//! `irq` feature is enabled.
//!
//! The implementation uses the `impl_trait!` helper to generate the FFI
//! shims expected by consumers. Documentation here focuses on the behavior
//! and expectations of each exported function.

use core::time::Duration;

use ax_memory_addr::MemoryAddr;
use axklib::{AxResult, IrqHandler, Klib, PhysAddr, VirtAddr, impl_trait};

struct KlibImpl;

fn dma_coherent_range(addr: VirtAddr, size: usize) -> Option<(VirtAddr, usize)> {
    if size == 0 {
        return None;
    }

    let start = addr.align_down_4k();
    let end = (addr + size).align_up_4k();
    Some((start, end - start))
}

#[cfg(target_arch = "aarch64")]
fn clean_invalidate_dcache_to_poc(addr: VirtAddr, size: usize) {
    use core::arch::asm;

    if size == 0 {
        return;
    }

    let line_size = ax_hal::asm::dcache_line_size_from_ctr();
    let start = addr.as_usize() & !(line_size - 1);
    let end = (addr.as_usize() + size + line_size - 1) & !(line_size - 1);
    for line in (start..end).step_by(line_size) {
        unsafe { asm!("dc civac, {0:x}", in(reg) line) };
    }
}

#[cfg(not(target_arch = "aarch64"))]
fn clean_invalidate_dcache_to_poc(_addr: VirtAddr, _size: usize) {}

#[cfg(target_arch = "aarch64")]
#[inline]
fn dsb_sy() {
    unsafe { core::arch::asm!("dsb sy") };
}

#[cfg(not(target_arch = "aarch64"))]
#[inline]
fn dsb_sy() {}

#[cfg(target_arch = "aarch64")]
#[inline]
fn isb_sy() {
    unsafe { core::arch::asm!("isb") };
}

#[cfg(not(target_arch = "aarch64"))]
#[inline]
fn isb_sy() {}

impl_trait! {
    impl Klib for KlibImpl {
        /// Map a physical region by delegating to the memory manager (`axmm`).
        ///
        /// This function forwards the request to `ax_mm::iomap` and returns the
        /// resulting virtual address wrapped in an `AxResult`.
        fn mem_iomap(addr: PhysAddr, size: usize) -> AxResult<VirtAddr> {
            // Convert from AxError (struct in ax_errno 0.2) to AxErrorKind (enum used by axklib)
            ax_mm::iomap(addr, size)
        }

        fn mem_virt_to_phys(addr: VirtAddr) -> PhysAddr {
            ax_hal::mem::virt_to_phys(addr)
        }

        fn mem_make_dma_coherent_uncached(addr: VirtAddr, size: usize) -> AxResult {
            let Some((start, size)) = dma_coherent_range(addr, size) else {
                return Ok(());
            };

            clean_invalidate_dcache_to_poc(start, size);
            dsb_sy();
            ax_mm::kernel_aspace().lock().protect(
                start,
                size,
                ax_hal::paging::MappingFlags::READ
                    | ax_hal::paging::MappingFlags::WRITE
                    | ax_hal::paging::MappingFlags::UNCACHED,
            )?;
            ax_hal::asm::flush_tlb(None);
            dsb_sy();
            isb_sy();
            Ok(())
        }

        fn mem_restore_dma_cached(addr: VirtAddr, size: usize) -> AxResult {
            let Some((start, size)) = dma_coherent_range(addr, size) else {
                return Ok(());
            };

            dsb_sy();
            ax_mm::kernel_aspace().lock().protect(
                start,
                size,
                ax_hal::paging::MappingFlags::READ | ax_hal::paging::MappingFlags::WRITE,
            )?;
            ax_hal::asm::flush_tlb(None);
            dsb_sy();
            isb_sy();
            Ok(())
        }

        fn dma_alloc_pages(dma_mask: u64, num_pages: usize, align: usize) -> AxResult<VirtAddr> {
            let addr = if dma_mask <= u32::MAX as u64 {
                ax_alloc::global_allocator().alloc_dma32_pages(
                    num_pages,
                    align,
                    ax_alloc::UsageKind::Dma,
                )
            } else {
                ax_alloc::global_allocator().alloc_pages(
                    num_pages,
                    align,
                    ax_alloc::UsageKind::Dma,
                )
            }?;
            Ok(VirtAddr::from(addr))
        }

        fn dma_dealloc_pages(addr: VirtAddr, num_pages: usize) {
            ax_alloc::global_allocator().dealloc_pages(
                addr.as_usize(),
                num_pages,
                ax_alloc::UsageKind::Dma,
            );
        }

        /// Busy-wait for the given duration by calling into `ax-hal`.
        ///
        /// Short delays are serviced by the hardware abstraction layer's
        /// busy-wait implementation. This is suitable for small spin waits
        /// but should not be used for long sleeps.
        fn time_busy_wait(dur: Duration) {
            ax_hal::time::busy_wait(dur);
        }

        fn time_monotonic_nanos() -> u64 {
            ax_hal::time::monotonic_time_nanos()
        }

        fn time_try_init_epoch_offset(epoch_time_nanos: u64) -> bool {
            ax_hal::time::try_init_epoch_offset(epoch_time_nanos)
        }

        /// Enable or disable the specified IRQ line.
        ///
        /// When the `irq` feature is enabled this forwards to
        /// `ax_hal::irq::set_enable`. Platforms built without IRQ support
        /// ignore this request because there is no interrupt controller
        /// service to program.
        fn irq_set_enable(_irq: usize, _enabled: bool) {
            #[cfg(feature = "irq")]
            ax_hal::irq::set_enable(_irq, _enabled);
        }

        /// Register an IRQ handler for the given IRQ number.
        ///
        /// Returns `true` when registration succeeds. With the `irq`
        /// feature enabled this delegates to `ax_hal::irq::register`. Without
        /// IRQ support registration fails explicitly by returning `false`.
        fn irq_register(_irq: usize, _handler: IrqHandler) -> bool {
            #[cfg(feature = "irq")]
            {
                ax_hal::irq::register(_irq, _handler)
            }
            #[cfg(not(feature = "irq"))]
            {
                false
            }
        }
    }
}
