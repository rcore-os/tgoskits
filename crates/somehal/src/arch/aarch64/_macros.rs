/// 生成 ADRP + ADD 指令组合，用于加载符号地址
/// ADRP 计算页面基地址，ADD 加上页内偏移
macro_rules! asm_sym_addr {
    ($reg:ident, $symbol:expr) => {
        concat!(
            "adrp ",
            stringify!($reg),
            ", ",
            $symbol,
            "\n",
            "add ",
            stringify!($reg),
            ", ",
            stringify!($reg),
            ", :lo12:",
            $symbol
        )
    };
}

macro_rules! sym_addr {
    ($sym:expr) => {{
        #[allow(unused_unsafe)]
        unsafe{
            let out: usize;
            core::arch::asm!(
                "adrp {r}, {s}",
                "add  {r}, {r}, :lo12:{s}",
                r = out(reg) out,
                s = sym $sym,
            );
            out
        }
    }};
}

macro_rules! ext_sym_addr {
    ($sym:expr) => {
        {
            #[allow(unused_unsafe)]
            unsafe{
                let out: usize;
                core::arch::asm!(
                    concat!("adrp {r}, ", stringify!($sym)),
                    concat!("add  {r}, {r}, :lo12:", stringify!($sym)),
                    r = out(reg) out,
                );
                out
            }
        }
    };
}
