//! Architecture helpers for FXMAC on supported targets.
//!
//! This module provides low-level helpers (CPU ID, barriers, cache ops) used by
//! the driver on aarch64 platforms.

#[cfg(target_arch = "aarch64")]
mod arch {
    use core::arch::asm;

    // PhytiumPi
    pub const CORE0_AFF: u64 = 0x200;
    pub const CORE1_AFF: u64 = 0x201;
    pub const CORE2_AFF: u64 = 0x00;
    pub const CORE3_AFF: u64 = 0x100;
    pub const FCORE_NUM: u64 = 4;

    /// Converts MPIDR to CPU ID
    pub(crate) fn mpidr2cpuid(mpidr: u64) -> usize {
        // RK3588
        //((mpidr >> 8) & 0xff) as usize

        // Qemu
        //(mpidr & 0xffffff & 0xff) as usize

        // PhytiumPi
        match (mpidr & 0xfff) {
            CORE0_AFF => 0,
            CORE1_AFF => 1,
            CORE2_AFF => 2,
            CORE3_AFF => 3,
            _ => {
                error!("Failed to get PhytiumPi CPU Id from mpidr={:#x}", mpidr);
                0
            }
        }
    }

    #[inline]
    /// Read reg: MPIDR_EL1
    fn read_mpidr() -> u64 {
        let mut reg_r = 0;
        unsafe {
            core::arch::asm!("mrs {}, MPIDR_EL1", out(reg) reg_r);
        }
        reg_r
    }

    pub(crate) fn get_cpu_id() -> usize {
        let mpidr = read_mpidr();
        mpidr2cpuid(mpidr)
    }

    /// Data Synchronization Barrier
    pub(crate) fn DSB() {
        unsafe {
            core::arch::asm!("dsb sy");
        }
    }

    #[inline]
    // pseudo assembler instructions
    fn MFCPSR() -> u32 {
        let mut rval: u32 = 0;
        unsafe {
            asm!("mrs {0:x}, DAIF", out(reg) rval);
        }
        rval
    }

    #[inline]
    fn MTCPSR(val: u32) {
        unsafe {
            asm!("msr DAIF, {0:x}", in(reg) val);
        }
    }
    #[inline]
    fn MTCPDC_CIVAC(adr: u64) {
        unsafe {
            asm!("dc CIVAC, {}", in(reg) adr);
        }
    }

    #[inline]
    fn MTCPDC_CVAC(adr: u64) {
        unsafe {
            asm!("dc CVAC, {}", in(reg) adr);
        }
    }

    /// CACHE of PhytiumPi
    pub const CACHE_LINE_ADDR_MASK: u64 = 0x3F;
    pub const CACHE_LINE: u64 = 64;

    /// Mask IRQ and FIQ interrupts in cpsr
    pub const IRQ_FIQ_MASK: u32 = 0xC0;

    /// dc civac, virt_addr 通过虚拟地址清除和无效化cache
    /// adr: 64bit start address of the range to be invalidated.
    /// len: Length of the range to be invalidated in bytes.
    pub(crate) fn FCacheDCacheInvalidateRange(mut adr: u64, len: u64) {
        let end: u64 = adr + len;
        adr &= !CACHE_LINE_ADDR_MASK;
        let currmask: u32 = MFCPSR();
        MTCPSR(currmask | IRQ_FIQ_MASK);
        if (len != 0) {
            while adr < end {
                MTCPDC_CIVAC(adr); /* Clean and Invalidate data cache by address to Point of Coherency */
                adr += CACHE_LINE;
            }
        }
        // Wait for invalidate to complete
        DSB();
        MTCPSR(currmask);
    }

    /// Flush Data cache
    /// DC CVAC, Virtual address to use. No alignment restrictions apply to vaddr
    /// adr: 64bit start address of the range to be flush.
    pub(crate) fn FCacheDCacheFlushRange(mut adr: u64, len: u64) {
        let end: u64 = adr + len;
        adr &= !CACHE_LINE_ADDR_MASK;
        let currmask: u32 = MFCPSR();
        MTCPSR(currmask | IRQ_FIQ_MASK);
        if len != 0 {
            while (adr < end) {
                MTCPDC_CVAC(adr); /* Clean data cache by address to Point of Coherency */
                adr += CACHE_LINE;
            }
        }
        // Wait for Clean to complete
        DSB();
        MTCPSR(currmask);
    }

    use aarch64_cpu::registers::{CNTFRQ_EL0, CNTVCT_EL0, Readable};

    #[inline]
    pub fn now_tsc() -> u64 {
        CNTVCT_EL0.get()
    }

    #[inline]
    pub fn timer_freq() -> u64 {
        CNTFRQ_EL0.get()
    }
}

#[cfg(not(target_arch = "aarch64"))]
mod arch {
    pub fn timer_freq() -> u64 {
        unimplemented!()
    }
    pub fn now_tsc() -> u64 {
        unimplemented!()
    }
    pub(crate) fn get_cpu_id() -> usize {
        unimplemented!()
    }
    pub(crate) fn DSB() {
        unimplemented!()
    }
    pub(crate) fn FCacheDCacheFlushRange(mut adr: u64, len: u64) {
        unimplemented!()
    }
    pub(crate) fn FCacheDCacheInvalidateRange(mut adr: u64, len: u64) {
        unimplemented!()
    }
}

use alloc::boxed::Box;

pub use arch::*;

// 纳秒(ns)
#[inline]
pub(crate) fn now_ns() -> u64 {
    let freq = timer_freq();
    now_tsc() * (1_000_000_000 / freq)
}

pub(crate) fn ticks_to_nanos(ticks: u64) -> u64 {
    let freq = timer_freq();
    ticks * (1_000_000_000 / freq)
}

// 微秒(us)
pub(crate) fn usdelay(us: u64) {
    let mut current_ticks: u64 = now_tsc();
    let delay2 = current_ticks + us * (timer_freq() / 1000000);

    while delay2 >= current_ticks {
        core::hint::spin_loop();
        current_ticks = now_tsc();
    }

    trace!("usdelay current_ticks: {}", current_ticks);
}

// 毫秒(ms)
pub(crate) fn msdelay(ms: u64) {
    usdelay(ms * 1000);
}

/// 虚拟地址转换成物理地址
#[linkage = "weak"]
#[unsafe(export_name = "virt_to_phys_fxmac")]
pub(crate) fn virt_to_phys(addr: usize) -> usize {
    debug!("fxmac: virt_to_phys_fxmac {:#x}", addr);
    addr
}

/// 物理地址转换成虚拟地址
#[linkage = "weak"]
#[unsafe(export_name = "phys_to_virt_fxmac")]
pub(crate) fn phys_to_virt(addr: usize) -> usize {
    debug!("fxmac: phys_to_virt_fxmac {:#x}", addr);
    addr
}

/// 申请DMA连续内存页
#[linkage = "weak"]
#[unsafe(export_name = "dma_alloc_coherent_fxmac")]
pub(crate) fn dma_alloc_coherent(pages: usize) -> (usize, usize) {
    let paddr: Box<[u32]> = if pages == 1 {
        Box::new([0; 1024]) // 4096
    } else if pages == 8 {
        Box::new([0; 1024 * 8]) // 4096
    } else {
        warn!("Alloc {} pages failed", pages);
        Box::new([0; 1024])
    };

    let len = paddr.len();

    let paddr = Box::into_raw(paddr) as *const u32 as usize;
    // let vaddr = phys_to_virt(paddr);
    let vaddr = paddr;
    debug!("fxmac: dma alloc paddr: {:#x}, len={}", paddr, len);

    (vaddr, paddr)
}

/// 释放DMA内存页
#[linkage = "weak"]
#[unsafe(export_name = "dma_free_coherent_fxmac")]
pub(crate) fn dma_free_coherent(vaddr: usize, pages: usize) {
    debug!("fxmac: dma free vaddr: {:#x}, pages={}", vaddr, pages);
    let palloc = vaddr as *mut [u32; 1024];
    unsafe {
        drop(Box::from_raw(palloc));
    }
}

/// 请求分配irq
#[linkage = "weak"]
#[unsafe(export_name = "dma_request_irq_fxmac")]
pub(crate) fn dma_request_irq(irq: usize, handler: fn()) {
    warn!("dma_request_irq_fxmac unimplemented");
    // unimplemented!()
}

// 路由中断到指定的cpu，或所有的cpu
// pub(crate) fn InterruptSetTargetCpus() {}
