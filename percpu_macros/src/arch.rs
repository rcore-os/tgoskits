use quote::{format_ident, quote};
use syn::{Ident, Type};

fn macos_unimplemented(item: proc_macro2::TokenStream) -> proc_macro2::TokenStream {
    quote! {
        {
            #[cfg(not(target_os = "macos"))]
            { #item }
            #[cfg(target_os = "macos")]
            unimplemented!()
        }
    }
}

/// Generate a code block that calculates the offset of the per-CPU variable based on the inner symbol name.
pub fn gen_offset(symbol: &Ident) -> proc_macro2::TokenStream {
    // the outer pair of braces is necessary to make the result an expression
    quote! {
        unsafe {
            let value: usize;
            #[cfg(target_arch = "x86_64")]
            ::core::arch::asm!(
                "mov {0:e}, offset {VAR}", // Requires offset <= 0xffff_ffff
                out(reg) value,
                VAR = sym #symbol,
            );
            #[cfg(target_arch = "aarch64")]
            ::core::arch::asm!(
                "movz {0}, #:abs_g0_nc:{VAR}", // Requires offset <= 0xffff
                out(reg) value,
                VAR = sym #symbol,
            );
            #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
            ::core::arch::asm!(
                "lui {0}, %hi({VAR})",
                "addi {0}, {0}, %lo({VAR})", // Requires offset <= 0xffff_ffff
                out(reg) value,
                VAR = sym #symbol,
            );
            #[cfg(any(target_arch = "loongarch64"))]
            ::core::arch::asm!(
                "lu12i.w {0}, %abs_hi20({VAR})",
                "ori {0}, {0}, %abs_lo12({VAR})", // Requires offset <= 0xffff_ffff
                out(reg) value,
                VAR = sym #symbol,
            );
            value
        }
    }
}

/// Generate a code block that calculates the pointer to the per-CPU variable on the current CPU, based on the inner
/// symbol name and the type of the variable.
pub fn gen_current_ptr(symbol: &Ident, ty: &Type) -> proc_macro2::TokenStream {
    let aarch64_tpidr = if cfg!(feature = "arm-el2") {
        "TPIDR_EL2"
    } else {
        // For ARM architecture, we assume running in EL1 by default,
        // and use `TPIDR_EL1` to store the base address of the per-CPU data area.
        "TPIDR_EL1"
    };
    let aarch64_asm = format!("mrs {{}}, {aarch64_tpidr}");

    macos_unimplemented(quote! {
        let base: usize;
        #[cfg(target_arch = "x86_64")]
        {
            // `__PERCPU_SELF_PTR` stores GS_BASE, which is defined in crate `percpu`.
            ::core::arch::asm!(
                "mov {0}, gs:[offset __PERCPU_SELF_PTR]",
                "add {0}, offset {VAR}",
                out(reg) base,
                VAR = sym #symbol,
            );
            base as *const #ty
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            #[cfg(target_arch = "aarch64")]
            ::core::arch::asm!(#aarch64_asm, out(reg) base);
            #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
            ::core::arch::asm!("mv {}, gp", out(reg) base);
            #[cfg(any(target_arch = "loongarch64"))]
            ::core::arch::asm!("move {}, $r21", out(reg) base);
            (base + self.offset()) as *const #ty
        }
    })
}

/// Generate a code block that reads the value of the per-CPU variable on the current CPU, based on the inner symbol
/// name and the type of the variable.
///
/// The type of the variable must be one of the following: `bool`, `u8`, `u16`, `u32`, `u64`, or `usize`.
pub fn gen_read_current_raw(symbol: &Ident, ty: &Type) -> proc_macro2::TokenStream {
    let ty_str = quote!(#ty).to_string();
    let rv64_op = match ty_str.as_str() {
        "u8" | "bool" => "lbu",
        "u16" => "lhu",
        "u32" => "lwu",
        "u64" | "usize" => "ld",
        _ => unreachable!(),
    };
    let rv64_asm = quote! {
        ::core::arch::asm!(
            "lui {0}, %hi({VAR})",
            "add {0}, {0}, gp",
            concat!(#rv64_op, " {0}, %lo({VAR})({0})"),
            out(reg) value,
            VAR = sym #symbol,
        )
    };

    // https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html#_ldx_buhuwud_stx_bhwd
    let la64_op = match ty_str.as_str() {
        "u8" | "bool" => "ldx.bu",
        "u16" => "ldx.hu",
        "u32" => "ldx.wu",
        "u64" | "usize" => "ldx.d",
        _ => unreachable!(),
    };
    let la64_asm = quote! {
        ::core::arch::asm!(
            "lu12i.w {0}, %abs_hi20({VAR})",
            "ori {0}, {0}, %abs_lo12({VAR})",
            concat!(#la64_op, " {0}, {0}, $r21"),
            out(reg) value,
            VAR = sym #symbol,
        )
    };

    let (x64_asm, x64_reg) = if ["bool", "u8"].contains(&ty_str.as_str()) {
        (
            "mov {0}, byte ptr gs:[offset {VAR}]".into(),
            format_ident!("reg_byte"),
        )
    } else {
        let (x64_mod, x64_ptr) = match ty_str.as_str() {
            "u16" => ("x", "word"),
            "u32" => ("e", "dword"),
            "u64" | "usize" => ("r", "qword"),
            _ => unreachable!(),
        };
        (
            format!("mov {{0:{x64_mod}}}, {x64_ptr} ptr gs:[offset {{VAR}}]"),
            format_ident!("reg"),
        )
    };
    let x64_asm = quote! {
        ::core::arch::asm!(#x64_asm, out(#x64_reg) value, VAR = sym #symbol)
    };

    let gen_code = |asm_stmt| {
        if ty_str.as_str() == "bool" {
            quote! {
                let value: u8;
                #asm_stmt;
                value != 0
            }
        } else {
            quote! {
                let value: #ty;
                #asm_stmt;
                value
            }
        }
    };

    let rv64_code = gen_code(rv64_asm);
    let la64_code = gen_code(la64_asm);
    let x64_code = gen_code(x64_asm);
    macos_unimplemented(quote! {
        #[cfg(target_arch = "riscv64")]
        { #rv64_code }
        #[cfg(target_arch = "loongarch64")]
        { #la64_code }
        #[cfg(target_arch = "x86_64")]
        { #x64_code }
        #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64", target_arch = "x86_64")))]
        { *self.current_ptr() }
    })
}

/// Generate a code block that writes the value of the per-CPU variable on the current CPU, based on the inner symbol
/// name, the identifier of the value to write, and the type of the variable.
///
/// The type of the variable must be one of the following: `bool`, `u8`, `u16`, `u32`, `u64`, or `usize`.
pub fn gen_write_current_raw(symbol: &Ident, val: &Ident, ty: &Type) -> proc_macro2::TokenStream {
    let ty_str = quote!(#ty).to_string();
    let ty_fixup = if ty_str.as_str() == "bool" {
        format_ident!("u8")
    } else {
        format_ident!("{}", ty_str)
    };

    let rv64_op = match ty_str.as_str() {
        "u8" | "bool" => "sb",
        "u16" => "sh",
        "u32" => "sw",
        "u64" | "usize" => "sd",
        _ => unreachable!(),
    };
    let rv64_code = quote! {
        ::core::arch::asm!(
            "lui {0}, %hi({VAR})",
            "add {0}, {0}, gp",
            concat!(#rv64_op, " {1}, %lo({VAR})({0})"),
            out(reg) _,
            in(reg) #val as #ty_fixup,
            VAR = sym #symbol,
        );
    };

    // https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html#common-memory-access-instructions
    let la64_op = match ty_str.as_str() {
        "u8" | "bool" => "stx.b",
        "u16" => "stx.h",
        "u32" => "stx.w",
        "u64" | "usize" => "stx.d",
        _ => unreachable!(),
    };
    let la64_code = quote! {
        ::core::arch::asm!(
            "lu12i.w {0}, %abs_hi20({VAR})",
            "ori {0}, {0}, %abs_lo12({VAR})",
            concat!(#la64_op, " {1}, {0}, $r21"),
            out(reg) _,
            in(reg) #val as #ty_fixup,
            VAR = sym #symbol,
        );
    };

    let (x64_asm, x64_reg) = if ["bool", "u8"].contains(&ty_str.as_str()) {
        (
            "mov byte ptr gs:[offset {VAR}], {0}".into(),
            format_ident!("reg_byte"),
        )
    } else {
        let (x64_mod, x64_ptr) = match ty_str.as_str() {
            "u16" => ("x", "word"),
            "u32" => ("e", "dword"),
            "u64" | "usize" => ("r", "qword"),
            _ => unreachable!(),
        };
        (
            format!("mov {x64_ptr} ptr gs:[offset {{VAR}}], {{0:{x64_mod}}}"),
            format_ident!("reg"),
        )
    };
    let x64_code = quote! {
        ::core::arch::asm!(#x64_asm, in(#x64_reg) #val as #ty_fixup, VAR = sym #symbol)
    };

    macos_unimplemented(quote! {
        #[cfg(target_arch = "riscv64")]
        { #rv64_code }
        #[cfg(target_arch = "loongarch64")]
        { #la64_code }
        #[cfg(target_arch = "x86_64")]
        { #x64_code }
        #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64", target_arch = "x86_64")))]
        { *(self.current_ptr() as *mut #ty) = #val }
    })
}
