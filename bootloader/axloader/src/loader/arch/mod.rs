//! Compile-time selection of the native EFI frontend.

cfg_select! {
    target_arch = "riscv64" => {
        mod riscv64;
        pub(super) use riscv64::run;
    }
    target_arch = "x86_64" => {
        mod x86_64;
        pub(super) use x86_64::run;
    }
}
