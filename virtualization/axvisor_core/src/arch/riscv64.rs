// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use ax_page_table_multiarch::riscv::SvVirtAddr;
use axaddrspace::GuestPhysAddr;
use axdevice_base::map_device_of_type;

use crate::vmm::vm_list::get_vm_by_id;

const GUEST_PLIC_PADDR: usize = 0x0c00_0000;

pub fn hardware_check() {
    // TODO: implement hardware checks for RISC-V64
    // check page table level like aarch64
}

pub fn hfence_vvma_all() {
    GuestPhysAddr::flush_tlb(None);
}

pub fn inject_current_interrupt(irq_id: usize) -> bool {
    let Some(context) = crate::context::try_current_vcpu_context() else {
        return false;
    };
    inject_interrupt(context.vm_id, irq_id)
}

pub fn inject_interrupt(vm_id: usize, irq_id: usize) -> bool {
    debug!("injecting interrupt id: {}", irq_id);

    let vplic = get_vm_by_id(vm_id)
        .unwrap()
        .get_devices()
        .find_mmio_dev(GuestPhysAddr::from_usize(GUEST_PLIC_PADDR))
        .unwrap();

    map_device_of_type(&vplic, |vplic: &riscv_vplic::VPlicGlobal| {
        match vplic.inject_irq(irq_id) {
            // The platform IRQ injector return value means "this host IRQ was
            // consumed by a guest", not "a new virtual pending bit was set".
            // Repeated level IRQs may already be pending/active in the vPLIC,
            // but they still belong to the guest and must not fall through to
            // the host unhandled-IRQ path.
            Ok(_) => true,
            Err(err) => {
                warn!("failed to inject interrupt id {irq_id} into guest vPLIC: {err:?}");
                false
            }
        }
    })
    .unwrap_or(false)
}

pub fn poll_host_plic(vm_id: usize) -> bool {
    let Some(vm) = get_vm_by_id(vm_id) else {
        return false;
    };
    let Some(vplic) = vm
        .get_devices()
        .find_mmio_dev(GuestPhysAddr::from_usize(GUEST_PLIC_PADDR))
    else {
        return false;
    };

    map_device_of_type(&vplic, |vplic: &riscv_vplic::VPlicGlobal| {
        if let Err(err) = vplic.poll_host_irqs() {
            warn!("failed to poll host PLIC for VM[{vm_id}]: {err:?}");
            return false;
        }
        true
    })
    .unwrap_or(false)
}
