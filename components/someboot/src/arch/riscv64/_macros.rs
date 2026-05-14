macro_rules! sym_addr {
    ($sym:expr) => {{
        let out: usize;
        #[allow(unused_unsafe)]
        unsafe {
            core::arch::asm!(
                "lla {out}, {sym}",
                out = out(reg) out,
                sym = sym $sym,
                options(nostack, preserves_flags),
            );
        }
        out
    }};
}

macro_rules! ext_sym_addr {
    ($sym:expr) => {{
        let out: usize;
        #[allow(unused_unsafe)]
        unsafe {
            core::arch::asm!(
                concat!("lla {out}, ", stringify!($sym)),
                out = out(reg) out,
                options(nostack, preserves_flags),
            );
        }
        out
    }};
}
