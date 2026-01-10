use core::{arch::naked_asm, ffi::c_void};

use crate::{arch::addrspace::*, mem::set_vm_load_offset, prime_entry, ArchTrait};

static FW_ARG0: usize = 0;
static FW_ARG1: usize = 0;
static FW_ARG2: usize = 0;

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
        // Enable PG
"
        li.w		$t0, 0xb0		    // PLV=0, IE=0, PG=1
        csrwr		$r12, {LOONGARCH_CSR_CRMD}
        li.w		$r12, 0x04		    // PLV=0, PIE=1, PWE=0
        csrwr		$r12, {LOONGARCH_CSR_PRMD}
        li.w		$r12, 0x00		    // FPE=0, SXE=0, ASXE=0, BTE=0
        csrwr		$t0, {LOONGARCH_CSR_EUEN}
",

"
	    la.pcrel	$t0, __bss_start		# clear .bss
	    st.d		$zero, $t0, 0
	    la.pcrel	$t1, __bss_stop - {LONGSIZE}
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
    // 在 relocate 之前获取内核代码的物理地址
    // 此时 ext_sym_addr!(_head) 返回的是实际加载地址（物理地址）
    let kernel_code_phys = super::Arch::kernel_code().as_ptr() as isize;

    // 执行重定位，将所有地址从物理地址转换为虚拟地址
    super::relocate();

    // 设置虚拟内存加载偏移量
    // kernel_code_phys 已经是物理地址，不需要再次转换
    // VM_LOAD_ADDRESS 是虚拟地址
    let offset = kernel_code_phys - VM_LOAD_ADDRESS as isize;
    set_vm_load_offset(offset);

    println!("Rust main.");

    if let Some(cmdline) = crate::cmdline::cmdline() {
        println!("{cmdline}");
    }

    prime_entry()
}
