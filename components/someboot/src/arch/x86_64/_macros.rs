macro_rules! sym_addr {
    ($sym:expr) => {{
        let out: usize;
        #[allow(unused_unsafe)]
        unsafe {
            core::arch::asm!(
                "lea {out}, [rip + {sym}]",
                out = out(reg) out,
                sym = sym $sym,
                options(nostack, preserves_flags),
            );
        }
        out
    }};
}
