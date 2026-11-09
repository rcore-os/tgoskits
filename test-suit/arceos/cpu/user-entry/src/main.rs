#![no_std]
#![no_main]
extern crate ax_std as std;

#[unsafe(no_mangle)]
fn main() {
    #[cfg(target_arch = "aarch64")]
    cpu_tests::user_entry::run();
    #[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
    cpu_tests::kernel_access::run();
    std::process::exit(0);
}
