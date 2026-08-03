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

#[test]
fn guest_run_preserves_host_daif_and_brackets_entry_with_host_hooks() {
    let vcpu = include_str!("../src/vcpu.rs");
    let run = vcpu
        .split_once("pub fn run(&mut self)")
        .expect("ArmVcpu::run must exist")
        .1
        .split_once("pub fn bind(&mut self)")
        .expect("ArmVcpu::bind must follow ArmVcpu::run")
        .0;

    let save = run
        .find("mrs {saved_daif}, daif")
        .expect("ArmVcpu::run must save the caller's complete DAIF state");
    let mask = run
        .find("msr daifset, #2")
        .expect("host IRQs must be masked before the guest-entry hook");
    let prepare = run
        .find("H::prepare_guest_entry()")
        .expect("the host must prepare passthrough IRQ delivery before guest entry");
    let enter = run
        .find("self.run_guest()")
        .expect("ArmVcpu::run must enter the guest");
    let complete = run
        .find("H::complete_guest_exit()")
        .expect("the host must reconcile passthrough IRQ delivery after guest exit");
    let handle_exit = run
        .find("self.vmexit_handler(trap_kind)")
        .expect("ArmVcpu::run must decode the guest exit");
    let restore = run
        .find("msr daif, {saved_daif}")
        .expect("ArmVcpu::run must restore the caller's exact DAIF state");

    assert!(save < mask && mask < prepare && prepare < enter);
    assert!(enter < complete && complete < handle_exit && handle_exit < restore);
    assert!(
        !run.contains("msr daifclr"),
        "ArmVcpu::run must not unconditionally enable host IRQs"
    );
}

#[test]
fn guest_entry_preserves_host_thread_pointer_before_vmexit_rust() {
    let vcpu = include_str!("../src/vcpu.rs");
    let run_guest = vcpu
        .split_once("unsafe extern \"C\" fn run_guest")
        .expect("ArmVcpu::run_guest must exist")
        .1
        .split_once("unsafe fn run_guest_panic")
        .expect("run_guest_panic must follow ArmVcpu::run_guest")
        .0;
    assert!(
        run_guest.contains("mrs x9, tpidr_el0"),
        "guest entry must save the host TPIDR_EL0"
    );

    let exception = include_str!("../src/exception.rs");
    let trampoline = exception
        .split_once("unsafe extern \"C\" fn vmexit_trampoline")
        .expect("the VM-exit trampoline must exist")
        .1
        .split_once("fn invalid_exception_el2")
        .expect("the invalid-exception handler must follow the trampoline")
        .0;
    let capture_guest = trampoline
        .find("mrs x10, tpidr_el0")
        .expect("VM exit must capture the guest TPIDR_EL0");
    let restore_host = trampoline
        .find("msr tpidr_el0, x11")
        .expect("VM exit must restore the host TPIDR_EL0");
    let return_to_rust = trampoline
        .find("restore_regs_from_stack!()")
        .expect("VM exit must restore the host call frame");
    assert!(capture_guest < restore_host && restore_host < return_to_rust);
}

#[test]
fn guest_thread_pointer_offset_accounts_for_guest_register_alignment() {
    let vcpu = include_str!("../src/vcpu.rs");
    let constants = vcpu
        .split_once("pub const ARM_VCPU_TRAP_FRAME_SIZE")
        .expect("the ArmVcpu layout constants must exist")
        .1
        .split_once("pub struct VmCpuRegisters")
        .expect("VmCpuRegisters must follow the ArmVcpu layout constants")
        .0;

    assert!(
        constants.contains("ARM_VCPU_GUEST_SYSTEM_REGISTERS_OFFSET"),
        "guest register offsets must share an explicit aligned base"
    );
    assert!(
        constants.contains("align_of::<GuestSystemRegisters>()"),
        "the guest register base must account for GuestSystemRegisters alignment"
    );
    assert!(
        constants.contains(
            "ARM_VCPU_GUEST_SYSTEM_REGISTERS_OFFSET\n    + \
             crate::context_frame::GUEST_SYSTEM_REGISTERS_TPIDR_EL0_OFFSET"
        ),
        "the TPIDR_EL0 slot must be relative to the aligned guest register base"
    );
}
