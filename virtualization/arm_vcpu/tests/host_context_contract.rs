// Copyright 2026 The Axvisor Team
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

const VCPU_SOURCE: &str = include_str!("../src/vcpu.rs");
const EXCEPTION_SOURCE: &str = include_str!("../src/exception.rs");

#[test]
fn guest_entry_saves_host_tpidr_el0() {
    assert!(VCPU_SOURCE.contains("tpidr_el0: u64"));
    assert!(VCPU_SOURCE.contains("mrs x9, tpidr_el0"));
    assert!(VCPU_SOURCE.contains("ARM_VCPU_HOST_TPIDR_EL0_OFFSET"));
}

#[test]
fn vm_exit_restores_host_tpidr_el0_before_returning_to_rust() {
    let trampoline = EXCEPTION_SOURCE
        .split_once("unsafe extern \"C\" fn vmexit_trampoline")
        .expect("VM exit trampoline must exist")
        .1;
    let restore = trampoline
        .find("msr tpidr_el0")
        .expect("VM exit must restore the host TPIDR_EL0");
    let return_to_host = trampoline
        .find("restore_regs_from_stack!()")
        .expect("VM exit must restore the host register frame");

    assert!(restore < return_to_host);
    assert!(trampoline.contains("ARM_VCPU_HOST_TPIDR_EL0_OFFSET"));
}

#[test]
fn vm_exit_preserves_guest_tpidr_el0_before_restoring_host_tls() {
    let trampoline = EXCEPTION_SOURCE
        .split_once("unsafe extern \"C\" fn vmexit_trampoline")
        .expect("VM exit trampoline must exist")
        .1;
    let save_guest = trampoline
        .find("mrs x11, tpidr_el0")
        .expect("VM exit must read the guest TPIDR_EL0");
    let restore_host = trampoline
        .find("msr tpidr_el0, x11")
        .expect("VM exit must restore the host TPIDR_EL0");

    assert!(save_guest < restore_host);
    assert!(trampoline.contains("ARM_VCPU_GUEST_TPIDR_EL0_OFFSET"));
    assert!(VCPU_SOURCE.contains("store(self.host.guest_tpidr_el0)"));
}
