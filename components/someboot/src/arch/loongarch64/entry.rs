use core::{arch::naked_asm, ffi::c_void, mem::offset_of};

use crate::{
    ArchTrait, arch::addrspace::*, entry::PrimaryCpuInitInfo, power::CpuOnError, smp::PerCpuMeta,
};

static mut FW_ARG0: usize = 0;
static mut FW_ARG1: usize = 0;
static mut FW_ARG2: usize = 0;

const MAX_SECONDARY_BOOT_ARGS: usize = 256;

#[repr(C)]
#[derive(Clone, Copy)]
struct SecondaryBootArg {
    hartid: usize,
    arg: usize,
}

impl SecondaryBootArg {
    const EMPTY: Self = Self {
        hartid: usize::MAX,
        arg: 0,
    };
}

static mut SECONDARY_BOOT_ARGS: [SecondaryBootArg; MAX_SECONDARY_BOOT_ARGS] =
    [SecondaryBootArg::EMPTY; MAX_SECONDARY_BOOT_ARGS];

pub(crate) fn set_secondary_boot_arg(hartid: usize, arg: usize) -> Result<(), CpuOnError> {
    let entries = core::ptr::addr_of_mut!(SECONDARY_BOOT_ARGS).cast::<SecondaryBootArg>();
    for idx in 0..MAX_SECONDARY_BOOT_ARGS {
        let entry = unsafe { entries.add(idx) };
        let stored_hartid = unsafe { (*entry).hartid };
        if stored_hartid == hartid || stored_hartid == usize::MAX {
            unsafe {
                (*entry).hartid = hartid;
                (*entry).arg = arg;
            }
            return Ok(());
        }
    }
    Err(CpuOnError::InvalidParameters)
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn kernel_entry(
    efi_boot: usize,
    cmdline: *const u8,
    systemtable: *const c_void,
) {
    naked_asm!(
    // SETUP_DMWINS
"
        li.d        $t0, {CSR_DMW0_INIT} // 0x8
        csrwr       $t0, {LOONGARCH_CSR_DMW0}
        li.d        $t0, {CSR_DMW1_INIT} // 0x9
        csrwr       $t0, {LOONGARCH_CSR_DMW1}
        li.d        $t0, {CSR_DMW2_INIT} // 0x9
        csrwr       $t0, {LOONGARCH_CSR_DMW2}
",
    // JUMP_TO_VIRT_ADDR
"
        li.d        $t0, {CACHE_BASE}
        pcaddi      $t1, 0
	    bstrins.d   $t0, $t1, ({DMW_PABITS} - 1), 0
        jirl        $zero, $t0, 0xc
",

    // Enable PG after jumping into the DMW-mapped address range.
"
        li.w		$t0, 0xb0		    // PLV=0, IE=0, PG=1
        csrwr		$t0, {LOONGARCH_CSR_CRMD}
        li.w		$r12, 0x04		    // PLV=0, PIE=1, PWE=0
        csrwr		$r12, {LOONGARCH_CSR_PRMD}
        li.w		$t0, 0x00		    // FPE=0, SXE=0, ASXE=0, BTE=0
        csrwr		$t0, {LOONGARCH_CSR_EUEN}
",

"
	la.pcrel	$t0, __bss_start		# clear .bss
	la.pcrel	$t1, __bss_stop
1:
	bgeu		$t0, $t1, 2f
	st.d		$zero, $t0, 0
	addi.d		$t0, $t0, {LONGSIZE}
	b		1b
2:
",

"
        la.pcrel	$t0, {fw_arg0}
	st.d		$a0, $t0, 0		# firmware arguments
	la.pcrel	$t0, {fw_arg1}
	st.d		$a1, $t0, 0
	la.pcrel	$t0, {fw_arg2}
	st.d		$a2, $t0, 0
",

"
        la.pcrel    $t0, __cpu0_stack_top
        addi.d      $sp, $t0, 0

        ibar        0
        dbar        0
        bl          {rust_main}
",

        CSR_DMW0_INIT = const CSR_DMW0_INIT,
        CSR_DMW1_INIT = const CSR_DMW1_INIT,
        CSR_DMW2_INIT = const CSR_DMW2_INIT,
        LOONGARCH_CSR_DMW0 = const 0x180,
        LOONGARCH_CSR_DMW1 = const 0x181,
        LOONGARCH_CSR_DMW2 = const 0x182,
        CACHE_BASE = const CACHE_BASE,
        DMW_PABITS = const DMW_DA_BITS,
        LOONGARCH_CSR_CRMD = const 0x0,
        LOONGARCH_CSR_PRMD = const 0x1,
        LOONGARCH_CSR_EUEN = const 0x2,
        LONGSIZE = const core::mem::size_of::<usize>(),
        fw_arg0 = sym FW_ARG0,
        fw_arg1 = sym FW_ARG1,
        fw_arg2 = sym FW_ARG2,
        rust_main = sym rust_main,
    )
}

fn rust_main() -> ! {
    // 执行重定位，将所有地址从物理地址转换为虚拟地址
    super::relocate();
    super::Arch::init_boot_tls();
    println!("LoongArch64 Rust kernel entry.");

    if is_boot_from_uefi() {
        efi_entry();
    }

    let kernel_code_start_lma = to_phys(sym_running_addr!(_head));
    println!("Kernel LMA: {:#x}", kernel_code_start_lma);
    let kernel_code_end_lma = to_phys(sym_running_addr!(__kernel_code_end));

    crate::entry::primary_init_early(PrimaryCpuInitInfo {
        kernel_start: kernel_code_start_lma.into(),
        kernel_end: kernel_code_end_lma.into(),
        kernel_start_link: VM_LOAD_ADDRESS.into(),
    });

    super::trap::per_cpu_trap_init(true);
    super::paging::relocate_kernel_to_vm_code()
}

fn efi_entry() {
    unsafe {
        crate::efi_stub::setup_service(FW_ARG2 as _);
        println!("UEFI setup.");
    }
}

pub(crate) fn mmu_entry() -> ! {
    crate::mem::mmu::set_mmu_enabled();
    println!("MMU ok...");
    crate::prime_entry()
}

fn is_boot_from_uefi() -> bool {
    unsafe { FW_ARG0 == 1 }
}

#[unsafe(naked)]
pub(crate) unsafe extern "C" fn _secondary_entry(_arg: usize) -> ! {
    naked_asm!(
        "
        li.d        $t0, {dmw0_init}
        csrwr       $t0, {csr_dmw0}
        li.d        $t0, {dmw1_init}
        csrwr       $t0, {csr_dmw1}
        li.d        $t0, {dmw2_init}
        csrwr       $t0, {csr_dmw2}

        csrrd       $a0, {csr_cpuid}
        la.pcrel    $t0, {secondary_boot_args}
        li.d        $t1, {max_secondary_boot_args}
    1:
        ld.d        $t2, $t0, {boot_arg_hartid_offset}
        ld.d        $t3, $t0, {boot_arg_arg_offset}
        beq         $t2, $a0, 2f
        addi.d      $t0, $t0, {boot_arg_size}
        addi.d      $t1, $t1, -1
        bnez        $t1, 1b

        li.w        $t0,  0x1028
        iocsrrd.d   $t3,  $t0
        beqz        $t3, 3f
    2:
        move        $a0, $t3

        li.d        $t0, {page_offset}
        add.d       $t1, $a0, $t0
        ld.d        $sp, $t1, {stack_top_offset}
        add.d       $sp, $sp, $t0

        ibar        0
        dbar        0
        bl          {enable_mmu_secondary}
    3:
        idle        0
        b           3b
        ",
        dmw0_init = const CSR_DMW0_INIT,
        dmw1_init = const CSR_DMW1_INIT,
        dmw2_init = const CSR_DMW2_INIT,
        csr_dmw0 = const 0x180,
        csr_dmw1 = const 0x181,
        csr_dmw2 = const 0x182,
        csr_cpuid = const 0x20,
        secondary_boot_args = sym SECONDARY_BOOT_ARGS,
        max_secondary_boot_args = const MAX_SECONDARY_BOOT_ARGS,
        boot_arg_hartid_offset = const offset_of!(SecondaryBootArg, hartid),
        boot_arg_arg_offset = const offset_of!(SecondaryBootArg, arg),
        boot_arg_size = const core::mem::size_of::<SecondaryBootArg>(),
        page_offset = const PAGE_OFFSET,
        stack_top_offset = const offset_of!(PerCpuMeta, stack_top),
        enable_mmu_secondary = sym super::paging::enable_mmu_secondary,
    )
}
