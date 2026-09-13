//! Architecture helpers for FXMAC on supported targets.
//!
//! This module provides low-level helpers (CPU ID and barriers) used by
//! the driver on aarch64 platforms.

#[cfg(target_arch = "aarch64")]
mod arch {

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
}

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

// 路由中断到指定的cpu，或所有的cpu
// pub(crate) fn InterruptSetTargetCpus() {}
