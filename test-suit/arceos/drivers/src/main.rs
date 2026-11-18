#![no_std]
#![no_main]
extern crate ax_std as std;
#[cfg(target_arch = "x86_64")]
mod acpica;
#[cfg(driver_batch = "riscv-aia")]
mod aplic;
#[cfg(target_arch = "x86_64")]
mod intel_hda;
#[cfg(driver_batch = "riscv-plic")]
mod plic;

// The selected calls come from the same declaration used by host planning.
include!(concat!(env!("OUT_DIR"), "/selected_drivers.rs"));

#[cfg(target_arch = "riscv64")]
struct Ingress;

#[cfg(target_arch = "riscv64")]
#[ax_crate_interface::impl_interface]
impl ax_plat::irq::riscv64_hv::RiscvHvIrqSink for Ingress {
    fn publish_physical_external_claim(_source: u32) -> bool {
        #[cfg(driver_batch = "riscv-aia")]
        return aplic::publish_physical_external_claim(_source);
        #[cfg(not(driver_batch = "riscv-aia"))]
        false
    }
}

#[unsafe(no_mangle)]
fn main() {
    run_selected_batch();
    std::process::exit(0);
}
