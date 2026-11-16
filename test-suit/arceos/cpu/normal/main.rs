#![no_std]
#![no_main]
extern crate ax_std as std;

// Only the chosen call sequence is generated; test implementations are library modules.
include!(concat!(env!("OUT_DIR"), "/selected_batch.rs"));

#[unsafe(no_mangle)]
fn main() {
    run_selected_batch();
    std::process::exit(0);
}
