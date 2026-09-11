#![no_std]
#![no_main]
extern crate ax_std as std;

core::cfg_select! {
    target_arch = "x86_64" => {
        mod x86_64;
        use x86_64 as current;
    }
    target_arch = "riscv64" => {
        mod riscv64;
        use riscv64 as current;
    }
    target_arch = "loongarch64" => {
        mod loongarch64;
        use loongarch64 as current;
    }
    target_arch = "aarch64" => {
        mod aarch64;
        use aarch64 as current;
    }
}

#[unsafe(no_mangle)]
fn main() {
    current::run();
}
