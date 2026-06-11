use core::{arch::global_asm, fmt::Arguments, mem::offset_of};

use loongArch64::register::{
    ecfg::{self},
    eentry, estat, tlbrentry,
};

use crate::{
    arch::{addrspace::to_phys, context::TrapFrame, register::csr},
    irq::IrqId,
};

/// LoongArch Exception Codes
#[allow(dead_code)]
mod exccode {
    /// TLB Refill - Page Invalid for Load
    pub const TLBL: usize = 0x1;
    /// TLB Refill - Page Invalid for Store
    pub const TLBS: usize = 0x2;
    /// TLB Refill - TLB Invalid for Load
    pub const TLBI: usize = 0x3;
    /// TLB Modify exception
    pub const TLBM: usize = 0x4;
    /// TLB No Read permission
    pub const TLBNR: usize = 0x5;
    /// TLB No Execute permission
    pub const TLBNX: usize = 0x6;
    /// TLB Privilege Error
    pub const TLBPE: usize = 0x7;
    /// Address Error - Fetch
    pub const ADF: usize = 0x8;
    /// Address Error - Memory access
    pub const ADE: usize = 0x8;
    /// Address Alignment Error - Load/Store
    pub const ALE: usize = 0x9;
    /// Bound Check Error
    pub const BCE: usize = 0xa;
    /// System Call
    pub const SYS: usize = 0xb;
    /// Breakpoint
    pub const BP: usize = 0xc;
    /// Reserved Instruction
    pub const INE: usize = 0xd;
    /// Instruction Privilege Error
    pub const IPE: usize = 0xe;
    /// FPU Disabled
    pub const FPDIS: usize = 0xf;
    /// 128-bit vector (LSX) Disabled
    pub const LSXDIS: usize = 0x10;
    /// 256-bit vector (LASX) Disabled
    pub const LASXDIS: usize = 0x11;
    /// Floating Point Exception
    pub const FPE: usize = 0x12;
    /// Watch Exception
    pub const WATCH: usize = 0x13;
    /// Binary Translation Disabled
    pub const BTDIS: usize = 0x14;
    /// TLB Refill (special, from TLBRENTRY)
    pub const TLBR: usize = 0x3f;
    /// Hardware Interrupt (start)
    pub const INT_START: usize = 64;
    /// Hardware Interrupt (end)
    pub const INT_END: usize = 64 + 14;
}

const VECSIZE: usize = 0x200;

use super::register::irq as cpuintc;

/// CPU 中断源数量 (SWI0-1, HWI0-7, PCOV, TI, IPI, NMI, AVEC)
const EXCCODE_INT_NUM: usize = 15;

/// 中断类型，包含硬件中断号
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqKind {
    /// CPU 本地中断 (SWI, HWI, TI, IPI, PMC, NMI, AVEC)
    /// hwirq 范围: 0-14，对应 ESTAT.IS 位
    Private(usize),
    /// 外部中断 (通过级联中断控制器)
    /// hwirq 为级联控制器的中断号
    External(usize),
}

impl IrqKind {
    /// 获取硬件中断号
    pub fn hwirq(&self) -> usize {
        match self {
            IrqKind::Private(hwirq) => *hwirq,
            IrqKind::External(hwirq) => *hwirq,
        }
    }

    /// 检查是否为私有中断
    pub fn is_private(&self) -> bool {
        matches!(self, IrqKind::Private(_))
    }

    /// 检查是否为外部中断
    pub fn is_external(&self) -> bool {
        matches!(self, IrqKind::External(_))
    }
}

impl IrqId {
    /// 创建 CPU 私有中断号
    /// hwirq: 硬件中断号 (0-14)
    pub fn private_irq(hwirq: usize) -> Self {
        debug_assert!(hwirq < EXCCODE_INT_NUM, "hwirq {hwirq} out of range");
        Self::new(hwirq)
    }

    /// 创建外部中断号
    /// 外部中断通过级联控制器路由，软件中断号 = hwirq + CPU_INT_NUM
    pub fn extern_irq(hwirq: usize) -> Self {
        Self::new(hwirq + EXCCODE_INT_NUM)
    }

    /// 获取中断类型及硬件中断号
    pub fn kind(&self) -> IrqKind {
        if self.raw() < EXCCODE_INT_NUM {
            IrqKind::Private(self.raw())
        } else {
            IrqKind::External(self.raw() - EXCCODE_INT_NUM)
        }
    }

    /// 检查是否为定时器中断
    pub fn is_timer(&self) -> bool {
        self.raw() == cpuintc::TI as usize
    }

    /// 检查是否为 IPI 中断
    pub fn is_ipi(&self) -> bool {
        self.raw() == cpuintc::IPI as usize
    }
}

// 从链接脚本获取异常向量表地址
unsafe extern "C" {
    fn __exception_vectors();
}

fn eentry_addr() -> usize {
    __exception_vectors as *const () as usize
}

fn tlbrentry_addr() -> usize {
    let addr = eentry_addr() + 80 * VECSIZE;
    to_phys(addr)
}

pub fn per_cpu_trap_init(_is_primary: bool) {
    setup_vint_size();
    configure_exception_vector();
}

fn setup_vint_size() {
    let n = (VECSIZE / 4).ilog2();
    ecfg::set_vs(n as _);
}

/// 配置异常向量
fn configure_exception_vector() {
    let eentry_addr = eentry_addr();
    println!("Setting EENTRY to {:#x}", eentry_addr);
    eentry::set_eentry(eentry_addr);
    let val = eentry::read().eentry();
    println!("EENTRY set to {:#x}", val);

    let tlbrentry_addr = tlbrentry_addr();
    println!("Setting TLBRENTRY to {:#x}", tlbrentry_addr);
    tlbrentry::set_tlbrentry(tlbrentry_addr);
}

/// 处理向量中断
fn do_vint(_tf: &mut TrapFrame) {
    let mut estat = estat::read().is();

    while estat != 0 {
        let hwirq = estat.trailing_zeros() + 1;
        estat &= !(1 << (hwirq - 1));

        unsafe extern "Rust" {
            fn _someboot_handle_irq(hwirq: usize);
        }

        unsafe { _someboot_handle_irq((hwirq - 1) as _) };
    }
}

/// Page Fault 处理函数 (普通 TLB 异常: TLBL, TLBS, TLBI, TLBM, TLBNR, TLBNX, TLBPE)
#[unsafe(no_mangle)]
extern "C" fn do_page_fault(tf: &TrapFrame, write: usize, address: usize) -> ! {
    println!("do_page_fault called");

    let estat = estat::read();
    let ecode = estat.ecode();
    let esubcode = estat.esubcode();

    let fault_type = match ecode {
        exccode::TLBL => "TLB Load Invalid",
        exccode::TLBS => "TLB Store Invalid",
        exccode::TLBI => "TLB Invalid",
        exccode::TLBM => "TLB Modify",
        exccode::TLBNR => "TLB No Read Permission",
        exccode::TLBNX => "TLB No Execute Permission",
        exccode::TLBPE => "TLB Privilege Error",
        _ => "Unknown Page Fault",
    };

    let access_type = if write != 0 { "write" } else { "read" };

    panic_on_exception(
        "PAGE FAULT",
        tf,
        format_args!(
            "Type:        {} (ecode=0x{:x}, esubcode=0x{:x})\nAccess:      {}\nAddress:     \
             0x{:016x}\nERA (PC):    0x{:016x}\nPRMD:        0x{:016x}",
            fault_type, ecode, esubcode, access_type, address, tf.era, tf.prmd
        ),
    )
}

/// TLB Refill 处理函数 (从 TLBRENTRY 入口触发)
/// TLB Refill 使用独立的 CSR: TLBRERA, TLBRPRMD, TLBRBADV
#[unsafe(no_mangle)]
extern "C" fn do_tlb_refill(tf: &TrapFrame, address: usize) -> ! {
    println!("do_tlb_refill called");

    panic_on_exception(
        "TLB REFILL",
        tf,
        format_args!(
            "Address:     0x{:016x}\nERA (PC):    0x{:016x}\nPRMD:        0x{:016x}",
            address, tf.era, tf.prmd
        ),
    )
}

// ============================================================================
// Exception Vector Table - 直接定义在 .exception.vectors 节中
// 每个向量间隔 VECSIZE (0x200 = 512 字节)
// ============================================================================

// TrapFrame 结构体的偏移常量
const TF_SP: usize = offset_of!(TrapFrame, regs.sp);
const TF_T0: usize = offset_of!(TrapFrame, regs.t0);
const TF_T1: usize = offset_of!(TrapFrame, regs.t1);
const TF_PRMD: usize = offset_of!(TrapFrame, prmd);
const TF_ERA: usize = offset_of!(TrapFrame, era);
const FRAME_SIZE: usize = size_of::<TrapFrame>();

global_asm!(
    // CSR 常量
    ".equ CSR_PRMD, {csr_prmd}",
    ".equ CSR_ERA, {csr_era}",
    ".equ CSR_BADV, {csr_badv}",
    ".equ CSR_KS0, 0x30",
    ".equ CSR_KS1, 0x31",
    ".equ CSR_TLBRBADV, 0x89",
    ".equ CSR_TLBRERA, 0x8a",
    ".equ CSR_TLBRPRMD, 0x8f",

    // TrapFrame 偏移
    ".equ TF_SP, {tf_sp}",
    ".equ TF_T0, {tf_t0}",
    ".equ TF_T1, {tf_t1}",
    ".equ TF_PRMD, {tf_prmd}",
    ".equ TF_ERA, {tf_era}",
    ".equ FRAME_SIZE, {frame_size}",

    // VECSIZE 常量
    ".equ VECSIZE, 0x200",

    // ========================================================================
    // 宏定义
    // ========================================================================
    r#"
.macro BACKUP_T0T1
    csrwr   $t0, CSR_KS0
    csrwr   $t1, CSR_KS1
.endm

.macro RESTORE_T0T1
    csrrd   $t0, CSR_KS0
    csrrd   $t1, CSR_KS1
.endm

.macro SAVE_REGS_EXCEPT_T0T1
    st.d    $zero, $sp, 0
    st.d    $ra, $sp, 8
    st.d    $tp, $sp, 16
    // skip sp (24) - saved separately
    st.d    $a0, $sp, 32
    st.d    $a1, $sp, 40
    st.d    $a2, $sp, 48
    st.d    $a3, $sp, 56
    st.d    $a4, $sp, 64
    st.d    $a5, $sp, 72
    st.d    $a6, $sp, 80
    st.d    $a7, $sp, 88
    // skip t0, t1 (96, 104) - saved separately
    st.d    $t2, $sp, 112
    st.d    $t3, $sp, 120
    st.d    $t4, $sp, 128
    st.d    $t5, $sp, 136
    st.d    $t6, $sp, 144
    st.d    $t7, $sp, 152
    st.d    $t8, $sp, 160
    st.d    $r21, $sp, 168
    st.d    $fp, $sp, 176
    st.d    $s0, $sp, 184
    st.d    $s1, $sp, 192
    st.d    $s2, $sp, 200
    st.d    $s3, $sp, 208
    st.d    $s4, $sp, 216
    st.d    $s5, $sp, 224
    st.d    $s6, $sp, 232
    st.d    $s7, $sp, 240
    st.d    $s8, $sp, 248
.endm

.macro RESTORE_REGS
    ld.d    $ra, $sp, 8
    ld.d    $tp, $sp, 16
    ld.d    $a0, $sp, 32
    ld.d    $a1, $sp, 40
    ld.d    $a2, $sp, 48
    ld.d    $a3, $sp, 56
    ld.d    $a4, $sp, 64
    ld.d    $a5, $sp, 72
    ld.d    $a6, $sp, 80
    ld.d    $a7, $sp, 88
    ld.d    $t0, $sp, 96
    ld.d    $t1, $sp, 104
    ld.d    $t2, $sp, 112
    ld.d    $t3, $sp, 120
    ld.d    $t4, $sp, 128
    ld.d    $t5, $sp, 136
    ld.d    $t6, $sp, 144
    ld.d    $t7, $sp, 152
    ld.d    $t8, $sp, 160
    ld.d    $r21, $sp, 168
    ld.d    $fp, $sp, 176
    ld.d    $s0, $sp, 184
    ld.d    $s1, $sp, 192
    ld.d    $s2, $sp, 200
    ld.d    $s3, $sp, 208
    ld.d    $s4, $sp, 216
    ld.d    $s5, $sp, 224
    ld.d    $s6, $sp, 232
    ld.d    $s7, $sp, 240
    ld.d    $s8, $sp, 248
.endm
"#,

    // ========================================================================
    // 开始异常向量表
    // ========================================================================
    ".section .exception.vectors, \"ax\"",
    ".balign 0x10000",              // 64KB 对齐

    // ------------------------------------------------------------------------
    // Vector 0: 保留异常
    // ------------------------------------------------------------------------
    ".balign VECSIZE",
    "handle_exc_0:",
    "b      handle_reserved_exception",

    // ------------------------------------------------------------------------
    // Vector 1: TLBL - TLB Load Invalid (Page Fault for Load)
    // ------------------------------------------------------------------------
    ".balign VECSIZE",
    "handle_exc_1:",
    "BACKUP_T0T1",
    "move    $t0, $sp",
    "addi.d  $sp, $sp, -FRAME_SIZE",
    "SAVE_REGS_EXCEPT_T0T1",
    "st.d    $t0, $sp, TF_SP",      // 保存原 SP
    "RESTORE_T0T1",
    "st.d    $t0, $sp, TF_T0",
    "st.d    $t1, $sp, TF_T1",
    "csrrd   $t0, CSR_PRMD",
    "st.d    $t0, $sp, TF_PRMD",
    "csrrd   $t0, CSR_ERA",
    "st.d    $t0, $sp, TF_ERA",
    "move    $a0, $sp",             // TrapFrame 指针
    "li.d    $a1, 0",               // write = 0 (读操作)
    "csrrd   $a2, CSR_BADV",        // 错误地址
    "bl      do_page_fault",
    // do_page_fault 是 noreturn，不会返回

    // ------------------------------------------------------------------------
    // Vector 2: TLBS - TLB Store Invalid (Page Fault for Store)
    // ------------------------------------------------------------------------
    ".balign VECSIZE",
    "handle_exc_2:",
    "BACKUP_T0T1",
    "move    $t0, $sp",
    "addi.d  $sp, $sp, -FRAME_SIZE",
    "SAVE_REGS_EXCEPT_T0T1",
    "st.d    $t0, $sp, TF_SP",
    "RESTORE_T0T1",
    "st.d    $t0, $sp, TF_T0",
    "st.d    $t1, $sp, TF_T1",
    "csrrd   $t0, CSR_PRMD",
    "st.d    $t0, $sp, TF_PRMD",
    "csrrd   $t0, CSR_ERA",
    "st.d    $t0, $sp, TF_ERA",
    "move    $a0, $sp",
    "li.d    $a1, 1",               // write = 1 (写操作)
    "csrrd   $a2, CSR_BADV",
    "bl      do_page_fault",

    // ------------------------------------------------------------------------
    // Vector 3: TLBI - TLB Invalid
    // ------------------------------------------------------------------------
    ".balign VECSIZE",
    "handle_exc_3:",
    "BACKUP_T0T1",
    "move    $t0, $sp",
    "addi.d  $sp, $sp, -FRAME_SIZE",
    "SAVE_REGS_EXCEPT_T0T1",
    "st.d    $t0, $sp, TF_SP",
    "RESTORE_T0T1",
    "st.d    $t0, $sp, TF_T0",
    "st.d    $t1, $sp, TF_T1",
    "csrrd   $t0, CSR_PRMD",
    "st.d    $t0, $sp, TF_PRMD",
    "csrrd   $t0, CSR_ERA",
    "st.d    $t0, $sp, TF_ERA",
    "move    $a0, $sp",
    "li.d    $a1, 0",
    "csrrd   $a2, CSR_BADV",
    "bl      do_page_fault",

    // ------------------------------------------------------------------------
    // Vector 4: TLBM - TLB Modify
    // ------------------------------------------------------------------------
    ".balign VECSIZE",
    "handle_exc_4:",
    "BACKUP_T0T1",
    "move    $t0, $sp",
    "addi.d  $sp, $sp, -FRAME_SIZE",
    "SAVE_REGS_EXCEPT_T0T1",
    "st.d    $t0, $sp, TF_SP",
    "RESTORE_T0T1",
    "st.d    $t0, $sp, TF_T0",
    "st.d    $t1, $sp, TF_T1",
    "csrrd   $t0, CSR_PRMD",
    "st.d    $t0, $sp, TF_PRMD",
    "csrrd   $t0, CSR_ERA",
    "st.d    $t0, $sp, TF_ERA",
    "move    $a0, $sp",
    "li.d    $a1, 1",
    "csrrd   $a2, CSR_BADV",
    "bl      do_page_fault",

    // ------------------------------------------------------------------------
    // Vector 5: TLBNR - TLB No Read Permission
    // ------------------------------------------------------------------------
    ".balign VECSIZE",
    "handle_exc_5:",
    "BACKUP_T0T1",
    "move    $t0, $sp",
    "addi.d  $sp, $sp, -FRAME_SIZE",
    "SAVE_REGS_EXCEPT_T0T1",
    "st.d    $t0, $sp, TF_SP",
    "RESTORE_T0T1",
    "st.d    $t0, $sp, TF_T0",
    "st.d    $t1, $sp, TF_T1",
    "csrrd   $t0, CSR_PRMD",
    "st.d    $t0, $sp, TF_PRMD",
    "csrrd   $t0, CSR_ERA",
    "st.d    $t0, $sp, TF_ERA",
    "move    $a0, $sp",
    "li.d    $a1, 0",
    "csrrd   $a2, CSR_BADV",
    "bl      do_page_fault",

    // ------------------------------------------------------------------------
    // Vector 6: TLBNX - TLB No Execute Permission
    // ------------------------------------------------------------------------
    ".balign VECSIZE",
    "handle_exc_6:",
    "BACKUP_T0T1",
    "move    $t0, $sp",
    "addi.d  $sp, $sp, -FRAME_SIZE",
    "SAVE_REGS_EXCEPT_T0T1",
    "st.d    $t0, $sp, TF_SP",
    "RESTORE_T0T1",
    "st.d    $t0, $sp, TF_T0",
    "st.d    $t1, $sp, TF_T1",
    "csrrd   $t0, CSR_PRMD",
    "st.d    $t0, $sp, TF_PRMD",
    "csrrd   $t0, CSR_ERA",
    "st.d    $t0, $sp, TF_ERA",
    "move    $a0, $sp",
    "li.d    $a1, 0",
    "csrrd   $a2, CSR_BADV",
    "bl      do_page_fault",

    // ------------------------------------------------------------------------
    // Vector 7: TLBPE - TLB Privilege Error
    // ------------------------------------------------------------------------
    ".balign VECSIZE",
    "handle_exc_7:",
    "BACKUP_T0T1",
    "move    $t0, $sp",
    "addi.d  $sp, $sp, -FRAME_SIZE",
    "SAVE_REGS_EXCEPT_T0T1",
    "st.d    $t0, $sp, TF_SP",
    "RESTORE_T0T1",
    "st.d    $t0, $sp, TF_T0",
    "st.d    $t1, $sp, TF_T1",
    "csrrd   $t0, CSR_PRMD",
    "st.d    $t0, $sp, TF_PRMD",
    "csrrd   $t0, CSR_ERA",
    "st.d    $t0, $sp, TF_ERA",
    "move    $a0, $sp",
    "li.d    $a1, 0",
    "csrrd   $a2, CSR_BADV",
    "bl      do_page_fault",

    // ------------------------------------------------------------------------
    // Vectors 8-63: Reserved exceptions -> handle_reserved_exception
    // 使用汇编宏生成
    // ------------------------------------------------------------------------
    r#"
.set exc_num, 8
.rept 1
    .balign VECSIZE
    .set handle_exc_label, exc_num
    BACKUP_T0T1
    move    $t0, $sp
    addi.d  $sp, $sp, -FRAME_SIZE
    SAVE_REGS_EXCEPT_T0T1
    st.d    $t0, $sp, TF_SP
    RESTORE_T0T1
    st.d    $t0, $sp, TF_T0
    st.d    $t1, $sp, TF_T1
    csrrd   $t0, CSR_PRMD
    st.d    $t0, $sp, TF_PRMD
    csrrd   $t0, CSR_ERA
    st.d    $t0, $sp, TF_ERA
    move    $a0, $sp
    csrrd   $a1, CSR_BADV
    bl      do_address_error
    .set exc_num, exc_num + 1
.endr
.rept 55
    .balign VECSIZE
    .set handle_exc_label, exc_num
    b handle_reserved_exception
    .set exc_num, exc_num + 1
.endr
"#,

    // ------------------------------------------------------------------------
    // Vectors 64-78: Hardware Interrupts (HWI0-HWI14)
    // ------------------------------------------------------------------------
    ".balign VECSIZE",
    "handle_vint_64:",
    "BACKUP_T0T1",
    "move    $t0, $sp",
    "addi.d  $sp, $sp, -FRAME_SIZE",
    "SAVE_REGS_EXCEPT_T0T1",
    "st.d    $t0, $sp, TF_SP",
    "RESTORE_T0T1",
    "st.d    $t0, $sp, TF_T0",
    "st.d    $t1, $sp, TF_T1",
    "csrrd   $t0, CSR_PRMD",
    "st.d    $t0, $sp, TF_PRMD",
    "csrrd   $t0, CSR_ERA",
    "st.d    $t0, $sp, TF_ERA",
    "move    $a0, $sp",
    "bl      {do_vint}",
    "ld.d    $t0, $sp, TF_ERA",
    "csrwr   $t0, CSR_ERA",
    "ld.d    $t0, $sp, TF_PRMD",
    "csrwr   $t0, CSR_PRMD",
    "RESTORE_REGS",
    "ld.d    $sp, $sp, TF_SP",
    "ertn",

    // 复制相同的中断处理代码到后续向量 (65-78)
    r#"
.set vint_num, 65
.rept 14
    .balign VECSIZE
    b handle_vint_64
    .set vint_num, vint_num + 1
.endr
"#,

    // 填充剩余向量 (79)
    ".balign VECSIZE",
    "handle_exc_79:", "b handle_reserved_exception",

    ".balign VECSIZE",
    ".global handle_tlb_refill",
    "handle_tlb_refill:",
    "
    csrwr   $t0, 0x8B  // LOONGARCH_CSR_TLBRSAV - 保存 $t0
    csrrd   $t0, 0x1b  // LA_CSR_PGD - 读取页表基址
    lddir   $t0, $t0, 3    // PGD 级别: level 3 → DIR2 (base=39)
    lddir   $t0, $t0, 2    // PUD 级别: level 2 → DIR1 (base=30)
    lddir   $t0, $t0, 1    // PMD 级别: level 1 → DIR0 (base=21)
    ldpte   $t0, 0         // PTE 级别: level 0 → PT (base=12)
    ldpte   $t0, 1         // PTE 对
    tlbfill                 // 填充 TLB
    csrrd   $t0, 0x8B  // LOONGARCH_CSR_TLBRSAV - 恢复 $t0
    ertn                    // 从异常返回
    ",
    // do_tlb_refill 是 noreturn，不会返回

    // 填充到 128 个向量 (81-127) - 使用 .rept 宏简化
    r#"
.set exc_num, 81
.rept 47
    .balign VECSIZE
    b handle_reserved_exception
    .set exc_num, exc_num + 1
.endr
"#,

    // ------------------------------------------------------------------------
    // 保留异常通用处理入口
    // ------------------------------------------------------------------------
    ".balign 16",
    "handle_reserved_exception:",
    "BACKUP_T0T1",
    "move    $t0, $sp",
    "addi.d  $sp, $sp, -FRAME_SIZE",
    "SAVE_REGS_EXCEPT_T0T1",
    "st.d    $t0, $sp, TF_SP",
    "RESTORE_T0T1",
    "st.d    $t0, $sp, TF_T0",
    "st.d    $t1, $sp, TF_T1",
    "csrrd   $t0, CSR_PRMD",
    "st.d    $t0, $sp, TF_PRMD",
    "csrrd   $t0, CSR_ERA",
    "st.d    $t0, $sp, TF_ERA",
    "move    $a0, $sp",
    "bl      do_reserved_exception",
    // do_reserved_exception 是 noreturn，不会返回

    csr_prmd = const csr::PRMD,
    csr_era = const csr::ERA,
    csr_badv = const csr::BADV,
    tf_sp = const TF_SP,
    tf_t0 = const TF_T0,
    tf_t1 = const TF_T1,
    tf_prmd = const TF_PRMD,
    tf_era = const TF_ERA,
    frame_size = const FRAME_SIZE,
    do_vint = sym do_vint,
);

/// 保留异常处理函数
#[unsafe(no_mangle)]
extern "C" fn do_reserved_exception(tf: &TrapFrame) -> ! {
    println!("*** do_reserved_exception 被调用 ***");
    let estat = estat::read();
    let ecode = estat.ecode();
    let esubcode = estat.esubcode();

    panic_on_exception(
        "RESERVED",
        tf,
        format_args!(
            "Ecode:       0x{:x}\nEsubcode:    0x{:x}\nERA (PC):    0x{:016x}\nPRMD:        \
             0x{:016x}",
            ecode, esubcode, tf.era, tf.prmd
        ),
    )
}

/// 地址错误异常处理函数 (ADF/ADE - Address Error)
///
/// LoongArch 中 ADF 和 ADE 都使用 Ecode 0x8
/// ADF: Address Error - Fetch (取指时地址错误)
/// ADE: Address Error - Memory access (内存访问时地址错误)
#[unsafe(no_mangle)]
extern "C" fn do_address_error(tf: &TrapFrame, badv: usize) -> ! {
    println!("\n*** do_address_error 被调用 ***");
    println!("BADV (错误地址): {:#x}", badv);

    let estat = estat::read();
    let ecode = estat.ecode();
    let esubcode = estat.esubcode();

    println!("ESTAT.ECODE: {:#x}", ecode);
    println!("ESTAT.ESUBCODE: {:#x}", esubcode);
    println!("ERA (PC): {:#x}", tf.era);

    // ADF 和 ADE 都使用 Ecode 0x8，这里统一处理
    // 根据子码或其他信息区分具体类型（如果需要）
    let fault_type = "Address Error";
    let _ = ecode; // 避免未使用警告

    println!("异常类型: {}", fault_type);
    println!("*** 开始 panic ***\n");

    panic_on_exception(
        "ADDRESS ERROR",
        tf,
        format_args!(
            "Type:        {} (ecode=0x{:x}, esubcode=0x{:x})\n\
            Address:     0x{:016x}\n\
            ERA (PC):    0x{:016x}\n\
            PRMD:        0x{:016x}",
            fault_type, ecode, esubcode, badv, tf.era, tf.prmd
        ),
    )
}

fn panic_on_exception(name: &str, tf: &TrapFrame, fmt: Arguments<'_>) -> ! {
    println!(
        "
        ============================================================\n{name} \
         EXCEPTION\n============================================================\n{fmt}
        \nGeneral Registers:\n------------------------------------------------------------\nra:  \
         0x{:016x}    tp:  0x{:016x}\nsp:  0x{:016x}    a0:  0x{:016x}\na1:  0x{:016x}    a2:  \
         0x{:016x}\na3:  0x{:016x}    a4:  0x{:016x}\na5:  0x{:016x}    a6:  0x{:016x}\na7:  \
         0x{:016x}    t0:  0x{:016x}\nt1:  0x{:016x}    t2:  0x{:016x}\nt3:  0x{:016x}    t4:  \
         0x{:016x}\nt5:  0x{:016x}    t6:  0x{:016x}\nt7:  0x{:016x}    t8:  0x{:016x}\nu0:  \
         0x{:016x}    fp:  0x{:016x}\ns0:  0x{:016x}    s1:  0x{:016x}\ns2:  0x{:016x}    s3:  \
         0x{:016x}\ns4:  0x{:016x}    s5:  0x{:016x}\ns6:  0x{:016x}    s7:  0x{:016x}\ns8:  \
         0x{:016x}\n============================================================\n",
        tf.regs.ra,
        tf.regs.tp,
        tf.regs.sp,
        tf.regs.a0,
        tf.regs.a1,
        tf.regs.a2,
        tf.regs.a3,
        tf.regs.a4,
        tf.regs.a5,
        tf.regs.a6,
        tf.regs.a7,
        tf.regs.t0,
        tf.regs.t1,
        tf.regs.t2,
        tf.regs.t3,
        tf.regs.t4,
        tf.regs.t5,
        tf.regs.t6,
        tf.regs.t7,
        tf.regs.t8,
        tf.regs.u0,
        tf.regs.fp,
        tf.regs.s0,
        tf.regs.s1,
        tf.regs.s2,
        tf.regs.s3,
        tf.regs.s4,
        tf.regs.s5,
        tf.regs.s6,
        tf.regs.s7,
        tf.regs.s8,
    );
    println!("Panicked:");
    panic!()
}
