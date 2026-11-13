#![no_std]
#![no_main]
extern crate ax_std as std;
mod acpica;
mod intel_hda;

// The selected calls come from the same declaration used by host planning.
include!(concat!(env!("OUT_DIR"), "/selected_drivers.rs"));

#[unsafe(no_mangle)]
fn main() {
    run_selected_batch();
    std::process::exit(0);
}
