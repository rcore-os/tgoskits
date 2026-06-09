#[cfg(feature = "arceos")]
use ax_std as _;

cfg_if::cfg_if! {
    if #[cfg(all(target_arch = "x86_64", feature = "x86-pc"))] {
        extern crate ax_plat_x86_pc;
    } else if #[cfg(all(target_arch = "loongarch64", feature = "loongarch64-qemu-virt"))] {
        extern crate ax_plat_loongarch64_qemu_virt;
    }
}

fn main() {
    println!("Hello, world!");
}
