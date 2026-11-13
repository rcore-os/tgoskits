#![no_std]
#![no_main]
extern crate ax_std as std;
#[cfg(target_arch = "x86_64")]
mod acpica;
#[cfg(target_arch = "riscv64")]
mod aplic;
#[cfg(target_arch = "x86_64")]
mod intel_hda;

// The selected calls come from the same declaration used by host planning.
include!(concat!(env!("OUT_DIR"), "/selected_drivers.rs"));

#[unsafe(no_mangle)]
fn main() {
    run_selected_batch();
    std::process::exit(0);
}
