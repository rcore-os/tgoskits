mod api;
pub mod cache;

use crate::vmm::vm_list::get_vm_by_id;
use axaddrspace::{GuestPhysAddr, device::AccessWidth};
use axdevice_base::map_device_of_type;
use axplat_riscv64_qemu_virt_hv::config::devices::PLIC_PADDR;
use axvisor_api::vmm::current_vm_id;
use riscv_vplic::VPlicGlobal;

pub fn hardware_check() {
    // TODO: implement hardware checks for RISC-V64
    // check page table level like aarch64
}

pub fn inject_interrupt(irq_id: usize) {
    debug!("injecting interrupt id: {}", irq_id);

    // Get the instance of the vplic, and then inject virtual interrupt.
    let vplic = get_vm_by_id(current_vm_id())
        .unwrap()
        .get_devices()
        .find_mmio_dev(GuestPhysAddr::from_usize(PLIC_PADDR))
        .unwrap();

    // Calulate the pending register offset and value.
    let reg_offset = riscv_vplic::PLIC_PENDING_OFFSET + (irq_id / 32) * 4;
    let addr = GuestPhysAddr::from_usize(PLIC_PADDR + reg_offset);
    let width = AccessWidth::Dword;
    let val: u32 = 1 << (irq_id % 32);

    // Use a trick write to set the pending bit.
    let _ = vplic.handle_write(addr, width, val as _);
}

pub fn bootstrap_passthrough_interrupts(vm_id: usize) {
    let Some(vm) = get_vm_by_id(vm_id) else {
        return;
    };
    let Some(vplic) = vm
        .get_devices()
        .find_mmio_dev(GuestPhysAddr::from_usize(PLIC_PADDR))
    else {
        return;
    };

    // Arm host-side PLIC passthrough only when a vCPU is about to run, so
    // early host boot IRQs are not injected before a VCpuTask context exists.
    let _ = map_device_of_type(&vplic, |vplic: &VPlicGlobal| {
        vplic.bootstrap_host_passthrough_plic();
    });
}
