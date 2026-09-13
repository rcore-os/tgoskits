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
pub(crate) mod trap;

use core::ptr::null;

pub(crate) use entry::_secondary_entry;
pub use paging::Entry;
pub use relocate::relocate;

use crate::{
    ArchTrait, DCacheOp,
    mem::{self, PageTableInfo},
    power::CpuOnError,
};

pub struct Arch;

impl ArchTrait for Arch {
    type P = paging::Generic;
    type Console = console::Console;

    fn _va(paddr: usize) -> *mut u8 {
        if mem::mmu::is_kernel_relocated() {
            paddr.wrapping_add(addrspace::PHYS_VIRT_OFFSET) as *mut u8
        } else {
            paddr as *mut u8
        }
    }

    fn _io(paddr: usize) -> *mut u8 {
        Self::_va(paddr)
    }

    fn cpu_area_phys_to_virt(paddr: usize) -> *mut u8 {
        (paddr + addrspace::PERCPU_BASE) as *mut u8
    }

    fn cpu_current_hartid() -> usize {
        ax_cpu::capability::CpuId::new()
            .get_feature_info()
            .map(|info| info.initial_local_apic_id() as usize)
            .unwrap_or(0)
    }

    fn jump_to(entry: usize, sp: usize) -> ! {
        // SAFETY: the boot owner supplies the final mapped stack and entry.
        unsafe { ax_cpu::boot::jump_to(entry, sp) }
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

    fn virtual_address_space()
    -> Result<crate::mem::VirtualAddressSpaceLayout, crate::mem::VirtualAddressSpaceError> {
        crate::mem::VirtualAddressSpaceLayout::try_new(
            crate::mem::configured_user_space(1usize << 47),
            addrspace::KERNEL_SPACE_BASE..usize::MAX,
        )
    }

    fn is_mmu_enabled() -> bool {
        unsafe { x86::controlregs::cr0().contains(x86::controlregs::Cr0::CR0_ENABLE_PAGING) }
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
        unsafe {
            x86::irq::disable();
            // QEMU ACPI poweroff ports (q35/i440fx).
            x86::io::outw(0x604, 0x2000);
            x86::io::outw(0xb004, 0x2000);
        }

        loop {
            ax_cpu::interrupt::halt();
        }
    }

    fn reset() -> ! {
        unsafe {
            x86::irq::disable();
            x86::io::outb(0x64, 0xfe);
        }

        loop {
            ax_cpu::interrupt::halt();
        }
    }

    fn secondary_entry_fn_address() -> *const () {
        _secondary_entry as *const ()
    }

    fn kick_secondary_cpu(hartid: usize, entry: usize, arg: usize) -> Result<(), CpuOnError> {
        power::kick_secondary_cpu(hartid, entry, arg)
    }

    fn systimer_freq() -> usize {
        trap::tsc_freq()
    }

    fn systimer_tick() -> usize {
        ax_cpu::timer::read_counter() as usize
    }

    fn systimer_stability() -> crate::timer::CounterStability {
        trap::scheduler_counter_stability()
    }

    fn dcache_range(_op: DCacheOp, _addr: usize, _size: usize) {
        ax_cpu::barrier::data_fence();
    }

    // Safety: `system_table` is forwarded from the EFI stub and must satisfy
    // the `ArchTrait::efi_enter_kernel` contract.
    unsafe fn efi_enter_kernel(system_table: *const ::core::ffi::c_void) -> bool {
        crate::arch::entry::kernel_entry(1, null(), system_table)
    }
}
