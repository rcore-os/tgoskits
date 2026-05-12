use quote::{format_ident, quote};
use syn::{Ident, ItemFn};

trait AsmFmt {
    fn fmt_asm(&self) -> Vec<String>;
}

impl AsmFmt for String {
    fn fmt_asm(&self) -> Vec<String> {
        self.split('\n')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}
pub fn trap_handle_irq(func: ItemFn) -> proc_macro2::TokenStream {
    let inner_name = format_ident!("__{}", func.sig.ident);
    let stmts = &func.block.stmts;
    let inputs = &func.sig.inputs;

    let fp = __trap_handle_irq(&func, &inner_name, true);
    let sp = __trap_handle_irq(&func, &inner_name, false);

    quote! {
        #[cfg(hard_float)]
        #fp

        #[cfg(not(hard_float))]
        #sp

        fn #inner_name(#inputs) {
            #(#stmts)*
        }
    }
}

pub fn __trap_handle_irq(
    func: &ItemFn,
    inner_name: &Ident,
    is_fp: bool,
) -> proc_macro2::TokenStream {
    let func_name = &func.sig.ident;
    let vis = &func.vis;

    let mut asm_str = trap_store_regs(is_fp);
    asm_str += "
    mov x0, sp
    BL {f}
    ";

    asm_str += &trap_restore_regs(is_fp);
    asm_str += "
    eret
    ";

    let asm = asm_str.fmt_asm();

    quote! {
        #[unsafe(no_mangle)]
        #[unsafe(naked)]
        #vis unsafe extern "C" fn #func_name() {
            core::arch::naked_asm!(
                #(#asm,)*
                f = sym #inner_name,
            );
        }
    }
}

pub fn tcb_switch(is_fp: bool) -> proc_macro2::TokenStream {
    let mut out = ctx_store_x_q(is_fp);
    // 保存 sp, lr
    out += "
    mov    x10, lr
    mov    x9, sp
    sub    x9, x9, #0x10
    stp    x9, x10, [sp, #-0x10]!

    mov    x8, sp
    str    x8, [x0, {sp_addr}] // prev.sp = sp
    ldr    x9, [x8, {lr_addr}] // x9 = prev.lr
    str    x9, [x8, {pc_addr}] // prev.pc = x9
    ldr    x8, [x1, {sp_addr}] // x8 = next.sp
    mov    sp, x8

    ldp    x9, lr,     [sp], #0x10
    ";

    out += &ctx_restore_x_q(is_fp);
    out += "
    ret
    ";
    let asm = out.fmt_asm();

    quote! {
        #[unsafe(no_mangle)]
        #[unsafe(naked)]
        pub unsafe extern "C" fn __tcb_switch(_prev: *mut u8, _next: *mut u8) {
            core::arch::naked_asm!(
                #(#asm,)*
                sp_addr = const core::mem::offset_of!(sparreal_kernel::task::TaskControlBlockData, sp),
                lr_addr = const core::mem::offset_of!(Context, lr),
                pc_addr = const core::mem::offset_of!(Context, pc)
            )
        }
    }
}

fn trap_store_regs(is_fp: bool) -> String {
    let mut out = ctx_store_x_q(is_fp);
    out += "
    mrs x10, ELR_EL1
    mov x9, sp
    sub x9, x9,   #0x10
	stp x9, x10,  [sp,#-0x10]!
        ";
    out
}

fn trap_restore_regs(is_fp: bool) -> String {
    let mut out = "
    ldp x0, x10,    [sp], #0x10
    msr	ELR_EL1,    x10
        "
    .to_string();
    out += &ctx_restore_x_q(is_fp);

    out
}

fn ctx_store_x_q(is_fp: bool) -> String {
    let mut out = reg_op_pair("stp", "x", 1..31, "[sp, #-0x10]!", true);
    out = format!(
        "{out}
    mrs    x9,     SPSR_EL1
    stp    x9, x0, [sp,#-0x10]!
"
    );

    if is_fp {
        out += &reg_op_pair("stp", "q", 0..32, "[sp, #-0x20]!", true);
        out += "
    mrs    x9,  FPCR
    mrs    x10, FPSR
    stp    x9,  x10, [sp, #-0x10]!
        "
    }
    out
}

fn ctx_restore_x_q(is_fp: bool) -> String {
    let mut out = String::new();
    if is_fp {
        out += "
    ldp    x9, x10, [sp], #0x10
    msr    FPCR,    x9
    msr    FPSR,    x10
        ";
        out += &reg_op_pair("ldp", "q", 0..32, "[sp], #0x20", false);
    }

    out += "
    ldp    x9, x0, [sp], #0x10
    msr    SPSR_EL1, x9
    ";

    out += &reg_op_pair("ldp", "x", 1..31, "[sp], #0x10", false);
    out
}

fn reg_op_pair(
    op: &str,
    reg: &str,
    range: std::ops::Range<usize>,
    after: &str,
    reverse: bool,
) -> String {
    let mut ls = range
        .step_by(2)
        .map(|p0| {
            format!(
                "{op} {:>3},{:>3},    {after}",
                format!("{reg}{p0}"),
                format!("{reg}{}", p0 + 1)
            )
        })
        .collect::<Vec<_>>();

    if reverse {
        ls.reverse();
    }

    ls.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reg_op_pair() {
        let want = "
ldp  X1, X2,    [sp], #0x10
ldp  X3, X4,    [sp], #0x10
ldp  X5, X6,    [sp], #0x10
ldp  X7, X8,    [sp], #0x10
ldp  X9,X10,    [sp], #0x10
ldp X11,X12,    [sp], #0x10
ldp X13,X14,    [sp], #0x10
ldp X15,X16,    [sp], #0x10
ldp X17,X18,    [sp], #0x10
ldp X19,X20,    [sp], #0x10
ldp X21,X22,    [sp], #0x10
ldp X23,X24,    [sp], #0x10
ldp X25,X26,    [sp], #0x10
ldp X27,X28,    [sp], #0x10
ldp X29,X30,    [sp], #0x10
";

        let a_str = reg_op_pair("ldp", "X", 1..31, "[sp], #0x10", false);

        println!("{a_str}");

        assert_eq!(a_str.trim(), want.trim());
    }

    #[test]
    fn test_asm_fmt() {
        let a = "
            str    x8, [x0, {sp_addr}] // prev.sp = sp
    ldr    x9, [x8, {lr_addr}] // x9 = prev.lr

    "
        .to_string();

        let asm = a.fmt_asm();

        println!("{asm:?}");
    }
}
