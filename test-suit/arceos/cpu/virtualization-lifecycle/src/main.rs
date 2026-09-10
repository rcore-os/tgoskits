#![no_std]
#![no_main]
extern crate ax_std as std;

core::cfg_select! {
    target_arch = "x86_64" => {
        mod x86_64;
        use x86_64::run;
    }
    target_arch = "aarch64" => {
        mod aarch64;
        use aarch64::run;
    }
    target_arch = "loongarch64" => {
        mod loongarch64;
        use loongarch64::run;
    }
}

#[unsafe(no_mangle)]
fn main() {
    run();
}
