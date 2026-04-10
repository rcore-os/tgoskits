#![cfg_attr(feature = "ax-std", no_std)]
#![cfg_attr(feature = "ax-std", no_main)]

cfg_if::cfg_if! {
    if #[cfg(all(target_arch = "aarch64", feature = "aarch64-qemu-virt"))] {
        extern crate ax_plat_aarch64_qemu_virt;
    } else if #[cfg(all(target_arch = "aarch64", feature = "aarch64-raspi4"))] {
        extern crate ax_plat_aarch64_raspi;
    } else if #[cfg(all(target_arch = "aarch64", feature = "aarch64-phytium-pi"))] {
        extern crate ax_plat_aarch64_phytium_pi;
    } else if #[cfg(all(target_arch = "aarch64", feature = "aarch64-bsta1000b"))] {
        extern crate ax_plat_aarch64_bsta1000b;
    } else if #[cfg(all(target_arch = "x86_64", feature = "x86-pc"))] {
        extern crate ax_plat_x86_pc;
    } else if #[cfg(all(target_arch = "riscv64", feature = "riscv64-qemu-virt"))] {
        extern crate ax_plat_riscv64_qemu_virt;
    } else if #[cfg(all(target_arch = "loongarch64", feature = "loongarch64-qemu-virt"))] {
        extern crate ax_plat_loongarch64_qemu_virt;
    } else {
        #[cfg(target_os = "none")] // ignore in rust-analyzer & cargo test
        compile_error!("No platform crate linked!\n\nPlease add `extern crate <platform>` in your code.");
    }
}

#[cfg(feature = "ax-std")]
use ax_std::println;

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    println!("Hello, world!");
}
