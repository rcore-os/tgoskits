//! Compile-time CPU backend selection.

core::cfg_select! {
    target_arch = "x86_64" => {
        pub(crate) mod x86_64;
        pub(crate) use x86_64 as current;
    }
    target_arch = "aarch64" => {
        pub(crate) mod aarch64;
        pub(crate) use aarch64 as current;
    }
    target_arch = "riscv64" => {
        pub(crate) mod riscv64;
        pub(crate) use riscv64 as current;
    }
    target_arch = "loongarch64" => {
        pub(crate) mod loongarch64;
        pub(crate) use loongarch64 as current;
    }
}

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64",
    target_arch = "loongarch64"
)))]
compile_error!("ax-cpu supports x86_64, aarch64, riscv64 and loongarch64 targets");
