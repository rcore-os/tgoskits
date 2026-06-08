mod api;
pub mod cache;

use crate::vmm::vm_list::get_vm_by_id;
use axvisor_api::vmm::current_vm_id;

pub fn hardware_check() {
    // TODO: implement hardware checks for RISC-V64
    // check page table level like aarch64
}

pub fn inject_interrupt(irq_id: usize) {
    debug!("injecting interrupt id: {}", irq_id);

    let vm = get_vm_by_id(current_vm_id()).unwrap();

    // Inject through the IRQ router, which dispatches to the vPLIC's
    // InterruptControllerOps::inject_irq (sets pending bit + syncs VSEIP).
    if let Err(e) = vm.router().inject(axbus::IrqMessage::Legacy {
        line: axbus::IrqLine(irq_id as u32),
    }) {
        warn!("inject_interrupt({irq_id}) failed: {e}");
    }
}
