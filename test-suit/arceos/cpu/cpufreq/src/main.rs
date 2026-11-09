#![no_std]
#![no_main]
extern crate ax_std as std;

#[unsafe(no_mangle)]
fn main() {
    cpu_tests::cpufreq::run();
    std::process::exit(0);
}
