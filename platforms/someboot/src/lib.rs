#![no_std]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(target_arch = "x86_64", feature(abi_x86_interrupt))]

#[allow(unused_imports)]
#[macro_use]
extern crate alloc;

#[macro_use]
extern crate core;

#[macro_use]
extern crate log;

#[macro_use]
pub mod console;

#[cfg(target_arch = "loongarch64")]
#[path = "arch/loongarch64/mod.rs"]
pub mod arch;

#[cfg(target_arch = "aarch64")]
#[path = "arch/aarch64/mod.rs"]
pub mod arch;

#[cfg(target_arch = "x86_64")]
#[path = "arch/x86_64/mod.rs"]
pub mod arch;

#[cfg(target_arch = "riscv64")]
#[path = "arch/riscv64/mod.rs"]
pub mod arch;

mod acpi;
mod cmdline;
pub(crate) mod consts;
#[cfg(efi)]
mod efi_stub;
mod elf;
mod entropy;
mod entry;
mod err;
pub(crate) mod fdt;
pub mod irq;
pub mod mem;
pub mod power;

pub mod rtc;
pub mod smp;
pub mod timer;

pub use acpi::rsdp_addr_phys;
pub use cmdline::cmdline;
pub use entropy::boot_entropy;
pub use fdt::{fdt_addr, fdt_addr_phys, platform_name};
pub use page_table_generic::*;
pub use somehal_macros::{entry, someboot_secondary_entry as secondary_entry};

use crate::{
    irq::IrqId,
    mem::{PageTableInfo, cpu_area_phys_to_virt},
    power::CpuOnError,
};

#[allow(unused)]
pub trait ArchTrait {
    type P: TableMeta;
    type Console: console::ArchConsoleOps;

    fn _va(paddr: usize) -> *mut u8;
    fn _io(paddr: usize) -> *mut u8 {
        Self::_va(paddr)
    }
    fn ioremap_device(_addr: usize, _size: usize) -> Option<*mut u8> {
        None
    }
    fn cpu_area_phys_to_virt(paddr: usize) -> *mut u8 {
        Self::_va(paddr)
    }

    fn cpu_current_hartid() -> usize;

    fn jump_to(entry: usize, sp: usize) -> !;

    fn post_allocator();

    fn init_boot_tls() {}

    fn per_cpu_trap_init(is_primary: bool);
    fn trap_addr() -> usize;

    fn virt_to_phys(vaddr: *const u8) -> usize;

    fn canonicalize_paddr(addr: usize) -> usize {
        addr
    }
    fn user_aspace_needs_kernel_mappings() -> bool {
        true
    }

    fn virtual_address_space()
    -> Result<mem::VirtualAddressSpaceLayout, mem::VirtualAddressSpaceError>;
    fn is_kernel_relocated_at(addr: usize) -> bool {
        (crate::consts::VM_LOAD_ADDRESS..usize::MAX).contains(&addr)
    }

    fn is_mmu_enabled() -> bool;

    fn kernel_page_table() -> PageTableInfo;
    fn set_kernel_page_table(val: PageTableInfo);
    #[cfg(uspace)]
    fn user_page_table() -> PageTableInfo;
    #[cfg(uspace)]
    fn set_user_page_table(val: PageTableInfo);

    fn shutdown() -> !;
    fn reset() -> ! {
        Self::shutdown()
    }
    fn secondary_entry_fn_address() -> *const ();
    /// Delivers the architecture-specific wake request to one secondary CPU.
    ///
    /// This method owns only the hardware or firmware transport. The generic
    /// someboot lifecycle publishes `KICKED`, waits for the target CPU to
    /// report `ALIVE`, and releases it into the OS entry path.
    fn kick_secondary_cpu(hartid: usize, entry: usize, arg: usize) -> Result<(), CpuOnError>;

    /// Get the timer frequency in Hz
    fn systimer_freq() -> usize;
    /// Get the current timer tick count
    fn systimer_tick() -> usize;
    /// Reports whether the timer counter is a synchronized system counter.
    fn systimer_stability() -> timer::CounterStability;

    fn irq_all_is_enabled() -> bool;
    fn irq_all_set_enable(enable: bool);

    fn dcache_range(op: DCacheOp, addr: usize, size: usize);

    /// Prepare cached pages before creating an uncached DMA alias.
    fn dma_coherent_before_map_uncached(addr: usize, size: usize) {
        Self::dcache_range(DCacheOp::CleanInvalidate, addr, size);
    }

    /// Order accesses before removing an uncached DMA alias.
    fn dma_coherent_before_unmap_uncached(_addr: usize, _size: usize) {}

    /// Complete ordering after a DMA coherent alias update.
    fn dma_coherent_after_mapping_update() {}

    /// EFI 入口点 - 从 EFI PE 入口跳转到内核
    ///
    /// Returns `false` on architectures without EFI handoff.
    ///
    /// # Safety
    /// `system_table` 必须是当前 EFI 固件提供的有效 `EFI_SYSTEM_TABLE` 指针，
    /// 并且调用者必须保证此调用符合对应架构的启动约定。
    unsafe fn efi_enter_kernel(_system_table: *const ::core::ffi::c_void) -> bool {
        false
    }
}

/// System-timer arming capability for architectures whose timer is hardware
/// independent of the interrupt controller (Arm generic timer, RISC-V SBI
/// timer, LoongArch TCG).
///
/// The counter domain (`systimer_freq`/`systimer_tick`/`systimer_stability`)
/// stays on [`ArchTrait`] because every architecture provides a counter. On
/// x86_64 the system timer lives inside the local APIC, so somehal's
/// interrupt-controller driver owns arming and the architecture simply does
/// not implement this trait — the absent capability is the compile-time
/// boundary, no conditional compilation is involved.
///
/// Primitives are implemented per architecture; the provided methods carry
/// the common implementations and may be overridden (LoongArch overrides the
/// per-line IRQ pair for its multi-line ECFG semantics).
pub trait SystimerArch: ArchTrait {
    /// The boot-level IRQ line of the system timer.
    fn systimer_irq_id() -> IrqId;
    fn systimer_enable();
    fn systimer_irq_enable();
    fn systimer_irq_disable();
    fn systimer_irq_is_enabled() -> bool;
    /// Set the timer interval in ticks.
    fn systimer_set_interval(ticks: usize);

    /// Acknowledge and clear the timer interrupt. Timers whose pending state
    /// clears on re-arming keep this default.
    fn systimer_ack() {}

    /// Whether one boot-level IRQ line is enabled. The common implementation
    /// knows only the system-timer line.
    fn irq_is_enabled(irq: IrqId) -> bool {
        irq == Self::systimer_irq_id() && Self::systimer_irq_is_enabled()
    }

    /// Enable or disable one boot-level IRQ line. The common implementation
    /// controls only the system-timer line and ignores others.
    fn irq_set_enable(irq: IrqId, enable: bool) {
        if irq == Self::systimer_irq_id() {
            if enable {
                Self::systimer_irq_enable();
            } else {
                Self::systimer_irq_disable();
            }
        }
    }

    /// Arms a one-shot deadline `ticks` from now.
    fn set_next_event_in_ticks(ticks: usize) {
        Self::systimer_set_interval(ticks);
    }

    /// Configure the system timer with the desired interval.
    fn set_next_event(interval: core::time::Duration) {
        const NANOS_PER_SEC: u128 = 1_000_000_000;
        let ticks = (interval.as_nanos() * Self::systimer_freq() as u128 / NANOS_PER_SEC) as usize;
        Self::systimer_set_interval(ticks);
    }
}

#[derive(Debug, Clone, Copy)]
pub enum DCacheOp {
    Clean,
    Invalidate,
    CleanInvalidate,
}

pub fn post_allocator() {
    fdt::init_with_alloc();
    smp::finalize_secondary_boot_metadata();
    debug!("Setup after allocator");
    arch::Arch::post_allocator();
}

/// Returns boot arguments captured from FDT, UEFI load options, or built into the image.
pub fn bootargs() -> Option<&'static str> {
    cmdline::cmdline()
}

/// Get the current kernel page table physical address and ASID
pub fn kernel_page_table_paddr() -> usize {
    arch::Arch::kernel_page_table().addr
}

/// Set the kernel page table physical address and ASID
pub fn set_kernel_page_table_paddr(paddr: usize) {
    arch::Arch::set_kernel_page_table(PageTableInfo {
        asid: 0,
        addr: paddr,
    });
}

#[cfg(uspace)]
pub fn user_page_table() -> PageTableInfo {
    arch::Arch::user_page_table()
}

#[cfg(uspace)]
pub fn set_user_page_table(pt: PageTableInfo) {
    arch::Arch::set_user_page_table(pt);
}

/// Entry point after enabling MMU
fn prime_entry() -> ! {
    fdt::setup_earlycon();
    let _ = acpi::earlycon::acpi_setup_earlycon();

    println!("Trap vector at {:#x}", arch::Arch::trap_addr());

    // mem::init_after_mmu();
    mem::memory_map_setup();
    mem::print_memory_map();

    smp::initialize_percpu_layout();

    unsafe extern "C" {
        fn __someboot_main() -> !;
    }

    let entry = __someboot_main as *const () as usize;
    let cpu_idx = crate::smp::early_current_cpu_idx();
    let sp = crate::smp::cpu_meta(cpu_idx).unwrap().stack_top;
    let sp = cpu_area_phys_to_virt(sp);
    println!(
        "Jumping to main entry point at {:#x} with SP {:#p}",
        entry, sp
    );
    arch::Arch::jump_to(entry, sp as _)
}
