use core::ffi::c_void;

use super::head::_head;
use crate::{entry::PrimaryCpuInitInfo, smp::PerCpuMeta};

unsafe extern "C" {
    fn __kernel_code_end();
}

#[unsafe(no_mangle)]
pub extern "C" fn kernel_entry(
    efi_boot: usize,
    _cmdline: *const u8,
    systemtable: *const c_void,
) -> ! {
    // Under UEFI, the firmware enters us after efi_stub initialized
    // uefi crate globals (image_handle/system_table). Clearing .bss here
    // would wipe that state before ExitBootServices.
    if efi_boot == 0 {
        clear_bss();
    }

    if efi_boot != 0 {
        crate::efi_stub::setup_service(systemtable);
        println!("UEFI setup.");
    }

    let kernel_code_start_lma = sym_addr!(_head);
    let kernel_code_end_lma = sym_addr!(__kernel_code_end);

    crate::entry::primary_init_early(PrimaryCpuInitInfo {
        kernel_start: kernel_code_start_lma.into(),
        kernel_end: kernel_code_end_lma.into(),
        kernel_start_link: crate::consts::VM_LOAD_ADDRESS.into(),
    });

    super::paging::enable_mmu()
}

pub(crate) fn mmu_entry() -> ! {
    super::relocate::reset();
    super::trap::setup();
    super::trap::init_local();
    crate::prime_entry()
}

pub(crate) unsafe extern "C" fn _secondary_entry(arg: usize) -> ! {
    let cpu_meta = unsafe { &*(crate::mem::phys_to_virt(arg) as *const PerCpuMeta) };
    super::power::notify_ap_started(cpu_meta.cpu_id);
    crate::entry::secondary_entry(cpu_meta);
    loop {
        core::hint::spin_loop();
    }
}

fn clear_bss() {
    unsafe extern "C" {
        static mut __bss_start: u8;
        static mut __bss_stop: u8;
    }

    let start = core::ptr::addr_of_mut!(__bss_start);
    let end = core::ptr::addr_of_mut!(__bss_stop);
    let len = end as usize - start as usize;
    unsafe {
        core::ptr::write_bytes(start, 0, len);
    }
}
