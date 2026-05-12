#![no_std]
#![no_main]
#![cfg(not(any(windows, unix)))]
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
mod entry;
mod err;
pub(crate) mod fdt;
pub mod irq;
pub mod mem;
pub mod power;
pub mod smp;
pub mod timer;

pub use fdt::{fdt_addr, fdt_addr_phys};
pub use page_table_generic::*;
pub use somehal_macros::{entry, irq_handler, someboot_secondary_entry as secondary_entry};

use crate::{
    irq::IrqId,
    mem::{__percpu, PageTableInfo},
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
    fn _percpu(paddr: usize) -> *mut u8 {
        Self::_va(paddr)
    }

    fn cpu_current_hartid() -> usize;

    fn jump_to(entry: usize, sp: usize) -> !;

    fn post_allocator();

    fn per_cpu_trap_init(is_primary: bool);
    fn trap_addr() -> usize;

    fn virt_to_phys(vaddr: *const u8) -> usize;

    fn kernel_space() -> core::ops::Range<usize>;

    fn kernel_page_table() -> PageTableInfo;
    fn set_kernel_page_table(val: PageTableInfo);
    #[cfg(uspace)]
    fn user_page_table() -> PageTableInfo;
    #[cfg(uspace)]
    fn set_user_page_table(val: PageTableInfo);

    fn shutdown() -> !;
    fn secondary_entry_fn_address() -> *const ();
    fn cpu_on(hartid: usize, entry: usize, arg: usize) -> Result<(), CpuOnError>;

    fn systimer_enable();
    fn systimer_irq_enable();
    fn systimer_irq_disable();
    fn systimer_irq_is_enabled() -> bool;
    /// Set the timer interval in ticks
    fn systimer_set_interval(ticks: usize);
    /// Acknowledge and clear the timer interrupt
    fn systimer_ack();
    /// Get the timer frequency in Hz
    fn systimer_freq() -> usize;
    /// Get the current timer tick count
    fn systimer_tick() -> usize;

    fn irq_all_is_enabled() -> bool;
    fn irq_all_set_enable(enable: bool);

    fn irq_is_enabled(irq: IrqId) -> bool;
    fn irq_set_enable(irq: IrqId, enable: bool);

    fn dcache_range(op: DCacheOp, addr: usize, size: usize);

    /// EFI 入口点 - 从 EFI PE 入口跳转到内核
    ///
    /// # Safety
    /// `system_table` 必须是当前 EFI 固件提供的有效 `EFI_SYSTEM_TABLE` 指针，
    /// 并且调用者必须保证此调用符合对应架构的启动约定。
    unsafe fn efi_enter_kernel(system_table: *const ::core::ffi::c_void) -> bool;
}

#[derive(Debug, Clone, Copy)]
pub enum DCacheOp {
    Clean,
    Invalidate,
    CleanInvalidate,
}

pub fn post_allocator() {
    fdt::init_with_alloc();
    smp::init_percpu();
    debug!("Setup after allocator");
    arch::Arch::post_allocator();
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

    unsafe extern "C" {
        fn __someboot_main() -> !;
    }

    let entry = __someboot_main as *const () as usize;
    let sp = crate::smp::cpu_meta(crate::smp::cpu_idx())
        .unwrap()
        .stack_top;
    let sp = __percpu(sp);
    println!(
        "Jumping to main entry point at {:#x} with SP {:#p}",
        entry, sp
    );
    arch::Arch::jump_to(entry, sp as _)
}
