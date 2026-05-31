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

use axaddrspace::{GuestPhysAddr, device::AccessWidth};
use axvisor_api::vmm::current_vm_id;

use crate::vmm::vm_list::get_vm_by_id;

const GUEST_PLIC_PADDR: usize = 0x0c00_0000;

pub fn hardware_check() {
    // TODO: implement hardware checks for RISC-V64
    // check page table level like aarch64
}

pub fn inject_interrupt(irq_id: usize) {
    debug!("injecting interrupt id: {}", irq_id);

    let vplic = get_vm_by_id(current_vm_id())
        .unwrap()
        .get_devices()
        .find_mmio_dev(GuestPhysAddr::from_usize(GUEST_PLIC_PADDR))
        .unwrap();

    let reg_offset = riscv_vplic::PLIC_PENDING_OFFSET + (irq_id / 32) * 4;
    let addr = GuestPhysAddr::from_usize(GUEST_PLIC_PADDR + reg_offset);
    let width = AccessWidth::Dword;
    let val: u32 = 1 << (irq_id % 32);

    let _ = vplic.handle_write(addr, width, val as _);
}
