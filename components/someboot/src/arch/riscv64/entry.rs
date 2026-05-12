use core::{arch::naked_asm, mem::offset_of};

use crate::{entry::PrimaryCpuInitInfo, smp::PerCpuMeta};

const RISCV_LINUX_IMAGE_TEXT_OFFSET: usize = 0x20_0000;
const RISCV_LINUX_IMAGE_FLAGS: usize = 0;
const RISCV_LINUX_IMAGE_VERSION: usize = 0x0000_0002;
const RISCV_LINUX_IMAGE_MAGIC: usize = 0x0056_4353_4952;
const RISCV_LINUX_IMAGE_MAGIC2: usize = 0x0543_5352;

#[unsafe(naked)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".head.text")]
pub unsafe extern "C" fn _head() -> ! {
    naked_asm!(
        ".option push",
        ".option norvc",
        // code0/code1
        "j {kernel_entry}",
        "nop",
        ".option pop",
        // text_offset
        ".quad {text_offset}",
        // image_size
        ".quad __kernel_load_end - _head",
        // flags
        ".quad {flags}",
        // version + reserved
        ".word {version}",
        ".word 0",
        // reserved
        ".quad 0",
        // magic + magic2 + reserved
        ".quad {magic}",
        ".word {magic2}",
        ".word 0",
        kernel_entry = sym kernel_entry,
        text_offset = const RISCV_LINUX_IMAGE_TEXT_OFFSET,
        flags = const RISCV_LINUX_IMAGE_FLAGS,
        version = const RISCV_LINUX_IMAGE_VERSION,
        magic = const RISCV_LINUX_IMAGE_MAGIC,
        magic2 = const RISCV_LINUX_IMAGE_MAGIC2,
    )
}

#[unsafe(naked)]
pub unsafe extern "C" fn kernel_entry(_hart_id: usize, _fdt_addr: usize) -> ! {
    naked_asm!(
        "mv tp, a0",
        "mv t0, a1",
        "lla sp, __cpu0_stack_top",
        "mv a0, t0",
        "j {primary_head_entry}",
        primary_head_entry = sym primary_head_entry,
    )
}

fn primary_head_entry(fdt_addr: usize) -> ! {
    super::relocate::apply();
    primary_entry(fdt_addr)
}

fn primary_entry(fdt_addr: usize) -> ! {
    clear_bss();
    unsafe {
        crate::fdt::FDT_ADDR = fdt_addr;
    }

    <<super::Arch as crate::ArchTrait>::Console as crate::console::ArchConsoleOps>::init();
    println!("RISC-V64 SBI kernel entry.");

    let kernel_start = super::kernel_load_address();

    crate::entry::primary_init_early(PrimaryCpuInitInfo {
        kernel_start: kernel_start.into(),
        kernel_end: (__kernel_code_end as *const () as usize).into(),
        kernel_start_link: crate::consts::VM_LOAD_ADDRESS.into(),
    });
    super::paging::enable_mmu()
}

pub(crate) fn mmu_entry() -> ! {
    super::relocate::reset();
    super::trap::setup();
    crate::prime_entry()
}

unsafe extern "C" {
    fn __kernel_code_end();
    fn __bss_start();
    fn __bss_stop();
}

#[unsafe(naked)]
pub(crate) unsafe extern "C" fn _secondary_entry(_hartid: usize, _cpu_meta_paddr: usize) -> ! {
    naked_asm!(
        "mv tp, a0",
        "mv t0, a1",
        "ld sp, {stack_top_offset}(t0)",
        "mv a0, t0",
        "j {secondary_start}",
        secondary_start = sym secondary_start,
        stack_top_offset = const offset_of!(PerCpuMeta, stack_top),
    )
}

fn secondary_start(cpu_meta_paddr: usize) -> ! {
    super::paging::enable_mmu_secondary(cpu_meta_paddr)
}

fn clear_bss() {
    let start = __bss_start as *const () as usize;
    let end = __bss_stop as *const () as usize;
    let len = end.saturating_sub(start);
    if len != 0 {
        unsafe {
            core::ptr::write_bytes(start as *mut u8, 0, len);
        }
    }
}
