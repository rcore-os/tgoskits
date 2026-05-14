#[macro_use]
mod _macros;

mod addrspace;
mod console;
mod entry;
pub(crate) mod irq;
mod paging;
mod relocate;
pub(crate) mod sbi;
mod trap;

use core::sync::atomic::{AtomicUsize, Ordering};

pub(crate) use entry::_secondary_entry;
use page_table_generic::{PageTableEntry, PhysAddr, PteConfig, TableMeta, VirtAddr};
pub use relocate::apply as relocate;

use crate::{
    ArchTrait, DCacheOp,
    mem::{PageTableInfo, mmu},
    power::CpuOnError,
};
#[cfg(uspace)]
use crate::{mem::__kimage_va_to_pa, smp::percpu_va_range};

const KERNEL_LOAD_ADDRESS: usize = 0x8020_0000;
const SATP_MODE_SV39: usize = 8usize << 60;
const SSTATUS_SIE: usize = 1 << 1;
const SIE_STIE: usize = 1 << 5;
const SV39_PPN_SHIFT: usize = 10;
const PTE_V: usize = 1 << 0;
const PTE_R: usize = 1 << 1;
const PTE_W: usize = 1 << 2;
const PTE_X: usize = 1 << 3;
const PTE_U: usize = 1 << 4;
const PTE_G: usize = 1 << 5;
const PTE_A: usize = 1 << 6;
const PTE_D: usize = 1 << 7;

static KERNEL_PAGE_TABLE_ADDR: AtomicUsize = AtomicUsize::new(0);
static TIMEBASE_FREQ: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, Default)]
#[repr(transparent)]
pub struct Entry(usize);

impl PageTableEntry for Entry {
    fn from_config(config: PteConfig) -> Self {
        if !config.valid {
            return Self(0);
        }

        let mut bits = PTE_V;
        let is_leaf = !config.is_dir || config.huge;
        if is_leaf {
            if config.read {
                bits |= PTE_R;
            }
            if config.writable {
                bits |= PTE_W;
            }
            if config.executable {
                bits |= PTE_X;
            }
            if config.lower {
                bits |= PTE_U;
            }
            if config.global {
                bits |= PTE_G;
            }
            if config.valid {
                bits |= PTE_A;
            }
            if config.writable || config.dirty {
                bits |= PTE_D;
            }
        }

        bits |= (config.paddr.raw() >> 12) << SV39_PPN_SHIFT;
        Self(bits)
    }

    fn to_config(&self, is_dir: bool) -> PteConfig {
        let bits = self.0;
        let valid = (bits & PTE_V) != 0;
        let read = (bits & PTE_R) != 0;
        let writable = (bits & PTE_W) != 0;
        let executable = (bits & PTE_X) != 0;
        let lower = (bits & PTE_U) != 0;
        let global = (bits & PTE_G) != 0;
        let dirty = (bits & PTE_D) != 0;
        let huge = is_dir && (read || writable || executable);
        let paddr = PhysAddr::new((bits >> SV39_PPN_SHIFT) << 12);

        PteConfig {
            paddr,
            valid,
            read,
            writable,
            executable,
            lower,
            dirty,
            global,
            is_dir,
            huge,
            mem_attr: Default::default(),
        }
    }

    fn valid(&self) -> bool {
        (self.0 & PTE_V) != 0
    }
}

#[derive(Clone, Copy)]
pub struct Generic;

impl TableMeta for Generic {
    type P = Entry;

    const PAGE_SIZE: usize = 0x1000;
    const LEVEL_BITS: &'static [usize] = &[9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 1;

    fn flush(_vaddr: Option<VirtAddr>) {
        unsafe {
            core::arch::asm!("sfence.vma zero, zero", options(nostack, preserves_flags));
        }
    }
}

pub struct Arch;

impl ArchTrait for Arch {
    type P = Generic;
    type Console = console::Console;

    fn _va(paddr: usize) -> *mut u8 {
        (paddr + addrspace::PAGE_OFFSET) as *mut u8
    }

    fn _percpu(paddr: usize) -> *mut u8 {
        (paddr + addrspace::PERCPU_BASE) as *mut u8
    }

    fn cpu_current_hartid() -> usize {
        let hart_id: usize;
        unsafe {
            core::arch::asm!("mv {hart_id}, tp", hart_id = out(reg) hart_id, options(nostack, preserves_flags));
        }
        hart_id
    }

    fn jump_to(entry: usize, sp: usize) -> ! {
        unsafe {
            core::arch::asm!(
                "mv sp, {sp}",
                "jr {entry}",
                sp = in(reg) sp,
                entry = in(reg) entry,
                options(noreturn)
            );
        }
    }

    fn post_allocator() {}

    fn per_cpu_trap_init(_is_primary: bool) {
        trap::setup();
    }

    fn trap_addr() -> usize {
        trap::trap_addr()
    }

    fn virt_to_phys(vaddr: *const u8) -> usize {
        let vaddr = vaddr as usize;
        #[cfg(uspace)]
        {
            if mmu::is_mmu_enabled() {
                if percpu_va_range().contains(&vaddr) {
                    return vaddr - addrspace::PERCPU_BASE;
                }
                if vaddr >= crate::consts::VM_LOAD_ADDRESS {
                    return __kimage_va_to_pa(vaddr as *const u8);
                }
                if vaddr >= addrspace::PAGE_OFFSET {
                    return vaddr - addrspace::PAGE_OFFSET;
                }
            }
        }
        vaddr
    }

    fn kernel_space() -> core::ops::Range<usize> {
        addrspace::PAGE_OFFSET..usize::MAX
    }

    fn kernel_page_table() -> PageTableInfo {
        if mmu::is_mmu_enabled() {
            current_page_table()
        } else {
            PageTableInfo {
                asid: 0,
                addr: KERNEL_PAGE_TABLE_ADDR.load(Ordering::Relaxed),
            }
        }
    }

    fn set_kernel_page_table(val: PageTableInfo) {
        KERNEL_PAGE_TABLE_ADDR.store(val.addr, Ordering::Relaxed);
        if mmu::is_mmu_enabled() {
            write_satp(val.addr);
        }
    }

    #[cfg(uspace)]
    fn user_page_table() -> PageTableInfo {
        PageTableInfo { asid: 0, addr: 0 }
    }

    #[cfg(uspace)]
    fn set_user_page_table(_val: PageTableInfo) {}

    fn shutdown() -> ! {
        let _ = sbi::system_reset_shutdown();
        loop {
            unsafe {
                core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
            }
        }
    }

    fn secondary_entry_fn_address() -> *const () {
        _secondary_entry as *const ()
    }

    fn cpu_on(hartid: usize, entry: usize, arg: usize) -> Result<(), CpuOnError> {
        if hartid == Self::cpu_current_hartid() {
            return Err(CpuOnError::AlreadyOn);
        }

        match sbi::hart_start(hartid, entry, arg) {
            Ok(()) => Ok(()),
            Err(sbi::HartStartError::AlreadyAvailable | sbi::HartStartError::AlreadyStarted) => {
                Err(CpuOnError::AlreadyOn)
            }
            Err(sbi::HartStartError::InvalidParam | sbi::HartStartError::InvalidAddress) => {
                Err(CpuOnError::InvalidParameters)
            }
            Err(sbi::HartStartError::NotSupported) => Err(CpuOnError::NotSupported),
            Err(sbi::HartStartError::Failed(err)) => Err(CpuOnError::Other(anyhow::anyhow!(
                "hart_start failed: {err:?}"
            ))),
        }
    }

    fn systimer_enable() {
        // Only bring the timer source into a known idle state here.
        // IRQ masking/unmasking is controlled separately by the timer core.
        let _ = sbi::set_timer(u64::MAX);
    }

    fn systimer_irq_enable() {
        unsafe {
            core::arch::asm!(
                "csrs sie, {stie}",
                stie = in(reg) SIE_STIE,
                options(nostack, preserves_flags)
            );
        }
    }

    fn systimer_irq_disable() {
        unsafe {
            core::arch::asm!(
                "csrc sie, {stie}",
                stie = in(reg) SIE_STIE,
                options(nostack, preserves_flags)
            );
        }
    }

    fn systimer_irq_is_enabled() -> bool {
        let sie: usize;
        unsafe {
            core::arch::asm!("csrr {sie}, sie", sie = out(reg) sie, options(nostack, preserves_flags));
        }
        (sie & SIE_STIE) != 0
    }

    fn systimer_set_interval(ticks: usize) {
        let now = Self::systimer_tick() as u64;
        let next = if ticks == usize::MAX {
            u64::MAX
        } else {
            now.saturating_add(ticks as u64).max(now + 1)
        };
        let _ = sbi::set_timer(next);
    }

    fn systimer_ack() {}

    fn systimer_freq() -> usize {
        let cached = TIMEBASE_FREQ.load(Ordering::Relaxed);
        if cached != 0 {
            return cached;
        }

        let freq = sbi::detect_timebase_frequency().unwrap_or(10_000_000);
        TIMEBASE_FREQ.store(freq, Ordering::Relaxed);
        freq
    }

    fn systimer_tick() -> usize {
        let ticks: usize;
        unsafe {
            core::arch::asm!("csrr {ticks}, time", ticks = out(reg) ticks, options(nostack, preserves_flags));
        }
        ticks
    }

    fn irq_all_is_enabled() -> bool {
        let sstatus: usize;
        unsafe {
            core::arch::asm!(
                "csrr {sstatus}, sstatus",
                sstatus = out(reg) sstatus,
                options(nostack, preserves_flags)
            );
        }
        (sstatus & SSTATUS_SIE) != 0
    }

    fn irq_all_set_enable(enable: bool) {
        unsafe {
            if enable {
                core::arch::asm!(
                    "csrs sstatus, {mask}",
                    mask = in(reg) SSTATUS_SIE,
                    options(nostack, preserves_flags)
                );
            } else {
                core::arch::asm!(
                    "csrc sstatus, {mask}",
                    mask = in(reg) SSTATUS_SIE,
                    options(nostack, preserves_flags)
                );
            }
        }
    }

    fn irq_is_enabled(irq: crate::irq::IrqId) -> bool {
        irq == irq::systimer_irq() && Self::systimer_irq_is_enabled()
    }

    fn irq_set_enable(irq: crate::irq::IrqId, enable: bool) {
        if irq == irq::systimer_irq() {
            if enable {
                Self::systimer_irq_enable();
            } else {
                Self::systimer_irq_disable();
            }
        }
    }

    fn dcache_range(_op: DCacheOp, _addr: usize, _size: usize) {
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack, preserves_flags));
        }
    }

    unsafe fn efi_enter_kernel(_system_table: *const ::core::ffi::c_void) -> bool {
        false
    }
}

pub(crate) fn kernel_load_address() -> usize {
    KERNEL_LOAD_ADDRESS
}

pub(crate) fn current_page_table() -> PageTableInfo {
    let satp: usize;
    unsafe {
        core::arch::asm!("csrr {satp}, satp", satp = out(reg) satp, options(nostack, preserves_flags));
    }
    let mode = satp >> 60;
    let addr = if mode == 0 {
        KERNEL_PAGE_TABLE_ADDR.load(Ordering::Relaxed)
    } else {
        (satp & ((1usize << 44) - 1)) << 12
    };
    PageTableInfo { asid: 0, addr }
}

pub(crate) fn write_satp(root_paddr: usize) {
    let satp = SATP_MODE_SV39 | (root_paddr >> 12);
    unsafe {
        core::arch::asm!(
            "csrw satp, {satp}",
            "sfence.vma zero, zero",
            satp = in(reg) satp,
            options(nostack, preserves_flags)
        );
    }
}
