#[macro_use]
mod _macros;

mod addrspace;
mod boot;
mod console;
mod entry;
pub(crate) mod irq;
mod paging;
mod pte;
pub use pte::{Entry, Generic};
mod relocate;
pub(crate) mod sbi;
mod trap;

use core::sync::atomic::{AtomicUsize, Ordering};

pub(crate) use entry::_secondary_entry;
pub use relocate::apply as relocate;

use crate::{
    ArchTrait, DCacheOp, SystimerArch,
    mem::{PageTableInfo, mmu},
    power::CpuOnError,
};
#[cfg(any(uspace, hv))]
use crate::{mem::__kimage_va_to_pa, smp::cpu_area_virtual_region};

static KERNEL_PAGE_TABLE_ADDR: AtomicUsize = AtomicUsize::new(0);
static TIMEBASE_FREQ: AtomicUsize = AtomicUsize::new(0);

pub struct Arch;

impl ArchTrait for Arch {
    type P = Generic;
    type Console = console::Console;

    fn _va(paddr: usize) -> *mut u8 {
        (paddr + addrspace::PAGE_OFFSET) as *mut u8
    }

    fn cpu_area_phys_to_virt(paddr: usize) -> *mut u8 {
        (paddr + addrspace::PERCPU_BASE) as *mut u8
    }

    fn cpu_current_hartid() -> usize {
        boot::current().hart_id()
    }

    fn jump_to(entry: usize, sp: usize) -> ! {
        // SAFETY: the boot owner retains the mapped entry and exclusive stack.
        unsafe { ax_cpu::boot::jump_to(0, sp.into(), entry.into()) }
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
        #[cfg(any(uspace, hv))]
        {
            if mmu::is_kernel_relocated() {
                if cpu_area_virtual_region().contains(&vaddr) {
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

    fn virtual_address_space()
    -> Result<crate::mem::VirtualAddressSpaceLayout, crate::mem::VirtualAddressSpaceError> {
        crate::mem::VirtualAddressSpaceLayout::try_new(
            crate::mem::configured_user_space(1usize << 38),
            addrspace::PAGE_OFFSET..usize::MAX,
        )
    }

    fn is_mmu_enabled() -> bool {
        current_satp_mode() != 0
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
            ax_cpu::interrupt::wait_for_irqs();
        }
    }

    fn reset() -> ! {
        let _ = sbi::system_reset_reboot();
        loop {
            ax_cpu::interrupt::wait_for_irqs();
        }
    }

    fn secondary_entry_fn_address() -> *const () {
        _secondary_entry as *const ()
    }

    fn kick_secondary_cpu(hartid: usize, entry: usize, arg: usize) -> Result<(), CpuOnError> {
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
        ax_cpu::timer::read_counter() as usize
    }

    fn systimer_stability() -> crate::timer::CounterStability {
        // The time CSR is a hart-local view of the platform-wide real-time
        // counter advertised by the firmware timebase.
        crate::timer::CounterStability::Stable
    }

    fn dcache_range(op: DCacheOp, addr: usize, size: usize) {
        #[cfg(feature = "thead-mae")]
        {
            thead_dcache_range(op, addr, size);
        }
        #[cfg(not(feature = "thead-mae"))]
        {
            let _ = (op, addr, size);
            riscv_dma_fence();
        }
    }
}

impl SystimerArch for Arch {
    fn systimer_irq_id() -> crate::irq::IrqId {
        irq::systimer_irq()
    }

    fn systimer_enable() {
        // Only bring the timer source into a known idle state here.
        // IRQ masking/unmasking is controlled separately by the timer core.
        Self::systimer_irq_disable();
        let _ = sbi::set_timer(u64::MAX);
    }

    fn systimer_irq_enable() {
        // SAFETY: the platform timer owner has installed its source handler.
        unsafe { ax_cpu::timer::set_irq_enabled(true) };
    }
    fn systimer_irq_disable() {
        // SAFETY: disabling this local source cannot admit a new interrupt.
        unsafe { ax_cpu::timer::set_irq_enabled(false) };
    }
    fn systimer_irq_is_enabled() -> bool {
        ax_cpu::timer::irq_enabled()
    }

    fn systimer_set_deadline(deadline_ticks: u64) {
        let now = Self::systimer_tick() as u64;
        let next = crate::timer::next_cpu_timer_deadline(now, deadline_ticks);
        let _ = sbi::set_timer(next);
    }

    fn systimer_requires_irq_quiesce() -> bool {
        false
    }

    fn systimer_cancel_oneshot() {
        Self::systimer_irq_disable();
        let _ = sbi::set_timer(crate::timer::riscv64_interval::stopped_deadline());
    }
}

#[cfg(feature = "thead-mae")]
fn thead_dcache_range(op: DCacheOp, vaddr: usize, size: usize) {
    use ax_cpu::cache::{PhysicalCacheRange, TheadDataCacheOperation};
    let paddr = Arch::virt_to_phys(vaddr as *const u8);
    let range = PhysicalCacheRange::new(paddr.into(), size).expect("DMA physical range wraps");
    if matches!(op, DCacheOp::Clean | DCacheOp::CleanInvalidate) {
        riscv_dma_fence();
    }
    let operation = if matches!(op, DCacheOp::Clean) {
        TheadDataCacheOperation::Clean
    } else {
        TheadDataCacheOperation::CleanInvalidate
    };
    // SAFETY: the platform selected T-Head support and retains the mapped DMA
    // buffer's physical lines until the surrounding direction fences complete.
    unsafe { ax_cpu::cache::maintain_thead_dcache(operation, range) };
    if matches!(op, DCacheOp::Invalidate | DCacheOp::CleanInvalidate) {
        riscv_dma_fence();
    }
}

fn riscv_dma_fence() {
    ax_cpu::barrier::data_fence();
}

pub(crate) fn disable_local_irqs() {
    ax_cpu::interrupt::disable_irqs();
}

pub(crate) fn current_page_table() -> PageTableInfo {
    let satp = ax_cpu::mmu::read_satp();
    let addr = if satp.mode() == ax_cpu::mmu::SatpMode::Bare {
        KERNEL_PAGE_TABLE_ADDR.load(Ordering::Relaxed)
    } else {
        satp.ppn() << 12
    };
    PageTableInfo {
        asid: satp.asid(),
        addr,
    }
}

fn current_satp_mode() -> usize {
    ax_cpu::mmu::read_satp().mode() as usize
}

pub(crate) fn write_satp(root_paddr: usize) {
    // SAFETY: boot retains the Sv39 root and executing mappings on this hart.
    unsafe {
        ax_cpu::mmu::install_page_table(
            ax_cpu::mmu::SatpMode::Sv39,
            ax_cpu::mmu::HardwareAddressSpace::new(root_paddr.into(), 0),
        )
    };
}
