//! Custom native EFI entry shims, selected before any Rust runtime setup.

cfg_select! {
    target_arch = "riscv64" => {
        mod riscv64;
    }
    target_arch = "loongarch64" => {
        mod loongarch64;
    }
}
