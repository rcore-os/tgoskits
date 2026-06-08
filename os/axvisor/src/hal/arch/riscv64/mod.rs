mod api;
pub mod cache;

use crate::vmm::vm_list::get_vm_by_id;
use axplat_riscv64_qemu_virt_hv::config::devices::PLIC_PADDR;
use axvisor_api::vmm::current_vm_id;

pub fn hardware_check() {
    // TODO: implement hardware checks for RISC-V64
    // check page table level like aarch64
}

pub fn inject_interrupt(irq_id: usize) {
    debug!("injecting interrupt id: {}", irq_id);

    let vm = get_vm_by_id(current_vm_id()).unwrap();

    // Write to the vPLIC pending register through the bus router.
    // This is equivalent to the old path but uses the unified router
    // instead of the deprecated get_devices().
    let reg_offset = riscv_vplic::PLIC_PENDING_OFFSET + (irq_id / 32) * 4;
    let addr = (PLIC_PADDR + reg_offset) as u64;
    let val: u32 = 1 << (irq_id % 32);

    vm.router().route(
        axbus::BusKind::Mmio,
        &axbus::BusAccess::Write {
            addr,
            width: axbus::AccessWidth::U32,
            val: val as u64,
        },
    );
}
