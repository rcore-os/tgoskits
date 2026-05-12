#[macro_use]
mod _macros;

mod addrspace;
mod console;
pub(crate) mod entry;
mod head;
pub(crate) mod irq;
mod paging;
pub(crate) mod power;
pub(crate) mod relocate;
mod trap;

use core::ptr::null;

pub(crate) use entry::_secondary_entry;
pub use paging::Entry;
pub use relocate::relocate;

use crate::{ArchTrait, DCacheOp, mem::PageTableInfo, power::CpuOnError};

pub struct Arch;

impl ArchTrait for Arch {
    type P = paging::Generic;
    type Console = console::Console;

    fn _va(paddr: usize) -> *mut u8 {
        paddr as *mut u8
    }

    fn _io(paddr: usize) -> *mut u8 {
        paddr as *mut u8
    }

    fn _percpu(paddr: usize) -> *mut u8 {
        (paddr + addrspace::PERCPU_BASE) as *mut u8
    }

    fn cpu_current_hartid() -> usize {
        x86::cpuid::CpuId::new()
            .get_feature_info()
            .map(|info| info.initial_local_apic_id() as usize)
            .unwrap_or(0)
    }

    fn jump_to(entry: usize, sp: usize) -> ! {
        unsafe {
            core::arch::asm!(
                "mov rsp, {sp}",
                "jmp {entry}",
                sp = in(reg) sp,
                entry = in(reg) entry,
                options(noreturn)
            );
        }
    }

    fn post_allocator() {}

    fn per_cpu_trap_init(_is_primary: bool) {
        trap::setup();
        trap::init_local();
    }

    fn trap_addr() -> usize {
        trap::trap_addr()
    }

    fn virt_to_phys(vaddr: *const u8) -> usize {
        paging::virt_to_phys(vaddr)
    }

    fn kernel_space() -> core::ops::Range<usize> {
        addrspace::KERNEL_SPACE_BASE..usize::MAX
    }

    fn kernel_page_table() -> PageTableInfo {
        paging::current_table()
    }

    fn set_kernel_page_table(val: PageTableInfo) {
        paging::set_table(val);
    }

    #[cfg(uspace)]
    fn user_page_table() -> PageTableInfo {
        PageTableInfo { asid: 0, addr: 0 }
    }

    #[cfg(uspace)]
    fn set_user_page_table(_val: PageTableInfo) {}

    fn shutdown() -> ! {
        // unsafe {
        //     x86::irq::disable();
        //     // QEMU ACPI poweroff ports (q35/i440fx).
        //     x86::io::outw(0x604, 0x2000);
        //     x86::io::outw(0xb004, 0x2000);
        // }

        if crate::efi_stub::is_uefi_available() {
            crate::efi_stub::reset(
                crate::efi_stub::ResetType::SHUTDOWN,
                crate::efi_stub::Status::SUCCESS,
                None,
            );
        }

        loop {
            unsafe { x86::halt() };
        }
    }

    fn secondary_entry_fn_address() -> *const () {
        _secondary_entry as *const ()
    }

    fn cpu_on(hartid: usize, entry: usize, arg: usize) -> Result<(), CpuOnError> {
        power::cpu_on(hartid, entry, arg)
    }

    fn systimer_enable() {
        trap::timer_enable();
    }

    fn systimer_irq_enable() {
        trap::timer_irq_enable();
    }

    fn systimer_irq_disable() {
        trap::timer_irq_disable();
    }

    fn systimer_irq_is_enabled() -> bool {
        trap::timer_irq_is_enabled()
    }

    fn systimer_set_interval(ticks: usize) {
        trap::timer_set_deadline_in_ticks(ticks);
    }

    fn systimer_ack() {
        trap::timer_ack();
    }

    fn systimer_freq() -> usize {
        trap::tsc_freq()
    }

    fn systimer_tick() -> usize {
        trap::ticks_now() as usize
    }

    fn irq_all_is_enabled() -> bool {
        trap::irq_local_enabled()
    }

    fn irq_all_set_enable(enable: bool) {
        trap::irq_local_set_enabled(enable);
    }

    fn irq_is_enabled(irq: crate::irq::IrqId) -> bool {
        irq == irq::systimer_irq() && trap::timer_irq_is_enabled()
    }

    fn irq_set_enable(irq: crate::irq::IrqId, enable: bool) {
        if irq == irq::systimer_irq() {
            if enable {
                trap::timer_irq_enable();
            } else {
                trap::timer_irq_disable();
            }
        }
    }

    fn dcache_range(_op: DCacheOp, _addr: usize, _size: usize) {
        unsafe {
            core::arch::asm!("mfence", options(nomem, nostack, preserves_flags));
        }
    }

    // Safety: `system_table` is forwarded from the EFI stub and must satisfy
    // the `ArchTrait::efi_enter_kernel` contract.
    unsafe fn efi_enter_kernel(system_table: *const ::core::ffi::c_void) -> bool {
        crate::arch::entry::kernel_entry(1, null(), system_table)
    }
}
