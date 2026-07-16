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

//! Source-level ownership contract for the AMD SVM world switch.
//!
//! `VMLOAD` installs guest FS/GS state, including the registers used by host
//! CPU-local and task-local access. Rust must therefore not run between the
//! guest `VMLOAD` and the host `VMLOAD` after `VMRUN`.

const SVM_VCPU: &str = include_str!("../src/svm/vcpu.rs");

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing `{start}`"))
        .1
        .split_once(end)
        .unwrap_or_else(|| panic!("missing `{end}` after `{start}`"))
        .0
}

#[test]
fn svm_world_switch_restores_host_fs_gs_before_returning_to_rust() {
    let switch = section(
        SVM_VCPU,
        "unsafe extern \"C\" fn svm_world_switch",
        "/// Host save area used to restore CPU state touched by SVM VMLOAD/VMSAVE.",
    );

    assert!(
        SVM_VCPU[..SVM_VCPU
            .find("unsafe extern \"C\" fn svm_world_switch")
            .expect("world-switch function")]
            .ends_with("#[unsafe(naked)]\n"),
        "the guest/host handoff must be a naked function"
    );
    assert!(switch.contains("naked_asm!"));

    let guest_load = switch.find("vmload rax").expect("guest VMLOAD");
    let run = switch.find("vmrun rax").expect("VMRUN");
    let guest_save = switch.find("vmsave rax").expect("guest VMSAVE");
    let host_load = switch
        .rfind("vmload rax")
        .filter(|offset| *offset != guest_load)
        .expect("host VMLOAD");
    let return_to_rust = switch.rfind("ret").expect("return to Rust");

    assert!(
        guest_load < run
            && run < guest_save
            && guest_save < host_load
            && host_load < return_to_rust,
        "the no-Rust window must be guest VMLOAD -> VMRUN -> guest VMSAVE -> host VMLOAD -> return"
    );
    assert_eq!(
        switch.match_indices("vmload rax").count(),
        2,
        "exactly one guest load and one host restore belong in the switch window"
    );
    assert!(
        !switch.contains("\"call "),
        "the world switch must not call Rust or helpers while guest FS/GS is live"
    );
}

#[test]
fn svm_world_switch_layout_and_call_site_are_explicit() {
    assert!(SVM_VCPU.contains("offset_of!(SvmWorldSwitchFrame, guest_regs)"));
    assert!(SVM_VCPU.contains("offset_of!(SvmWorldSwitchFrame, host_stack_top)"));
    assert!(SVM_VCPU.contains("offset_of!(SvmWorldSwitchFrame, host_rflags)"));
    assert!(SVM_VCPU.contains("offset_of!(SvmWorldSwitchFrame, host_vmcb_pa)"));

    let prepare = section(SVM_VCPU, "fn prepare_world_switch", "unsafe fn svm_run");
    assert!(
        !prepare.contains("instructions::vmload"),
        "preparation must not install guest FS/GS before entering assembly"
    );

    let run = section(SVM_VCPU, "unsafe fn svm_run", "\n    }\n}");
    assert!(run.contains("svm_world_switch("));
    assert!(!run.contains("instructions::vmload"));
    assert!(!run.contains("instructions::vmsave"));
}

#[test]
fn svm_guest_xstate_is_restored_before_host_interrupts() {
    let run = section(
        SVM_VCPU,
        "unsafe fn svm_run",
        "fn inject_external_interrupt_control",
    );

    let disable = run
        .find("capture_and_disable_host_interrupts")
        .expect("host IRQ state must be captured before switching xstate");
    let capture = run
        .find("self.world_switch.xstate.capture_host()")
        .expect("host xstate capture");
    let world_switch = run.find("svm_world_switch(").expect("world switch");
    let gif = run
        .rfind("instructions::stgi")
        .expect("host GIF restoration");
    let interrupt_flag = run
        .rfind("restore_host_interrupt_flag")
        .expect("host IF restoration");

    assert!(
        disable < capture && capture < world_switch && world_switch < gif && gif < interrupt_flag,
        "guest xstate must be confined to the IRQ/GIF-off world-switch window"
    );

    let switch = section(
        SVM_VCPU,
        "unsafe extern \"C\" fn svm_world_switch",
        "/// Host save area used to restore CPU state touched by SVM VMLOAD/VMSAVE.",
    );
    assert!(
        !switch.contains("pushfq"),
        "the assembly must preserve flags captured before guest xstate became live"
    );
    let guest_xstate = switch
        .find("install_guest_xstate_from_rdi!()")
        .expect("guest xstate install");
    let guest_load = switch.find("vmload rax").expect("guest VMLOAD");
    let host_load = switch.rfind("vmload rax").expect("host VMLOAD");
    let host_xstate = switch
        .find("restore_host_xstate_from_rdi!()")
        .expect("host xstate restore");
    assert!(
        guest_xstate < guest_load && host_load < host_xstate,
        "xstate ownership must change only inside the assembly world switch"
    );
}

#[test]
fn svm_vmrun_observes_if_set_without_an_sti_shadow() {
    let run = section(
        SVM_VCPU,
        "unsafe fn svm_run",
        "fn inject_external_interrupt_control",
    );
    let clgi = run
        .find("self.prepare_world_switch()")
        .expect("CLGI preparation");
    let enable = run
        .find("x86_64::instructions::interrupts::enable()")
        .expect("host IF enable while GIF is clear");
    let world_switch = run.find("svm_world_switch(").expect("world switch");
    let disable = run
        .rfind("x86_64::instructions::interrupts::disable()")
        .expect("host IF disable before STGI");
    let stgi = run.rfind("instructions::stgi").expect("host GIF restore");

    assert!(
        clgi < enable && enable < world_switch && world_switch < disable && disable < stgi,
        "VMRUN must observe IF=1 while GIF=0, and STGI must observe IF=0"
    );

    let switch = section(
        SVM_VCPU,
        "unsafe extern \"C\" fn svm_world_switch",
        "/// Host save area used to restore CPU state touched by SVM VMLOAD/VMSAVE.",
    );
    assert!(
        !switch.contains("\"sti\""),
        "IF must be enabled before the naked switch to avoid leaking an STI shadow into the guest"
    );
}

#[test]
fn svm_guest_xsetbv_is_bounded_by_the_kernel_managed_mask() {
    let handler = section(SVM_VCPU, "fn handle_xsetbv", "fn svm_io_exit_info");
    assert!(
        handler.contains("validate_guest_xsetbv"),
        "both backends must use the shared CPUID/XCR0 capability closure"
    );
    assert!(handler.contains("INVALID_OPCODE_VECTOR"));
}

#[test]
fn svm_guest_cpuid_and_xss_share_the_managed_xstate_policy() {
    let cpuid = section(SVM_VCPU, "fn handle_cpuid", "fn handle_xsetbv");
    assert!(
        cpuid.contains("filter_guest_cpuid"),
        "all xstate-dependent feature leaves must use the shared policy"
    );

    let bitmap = section(SVM_VCPU, "fn setup_msr_bitmap", "fn setup_io_bitmap");
    assert!(bitmap.contains("set_read_intercept(IA32_XSS_MSR, true)"));
    assert!(bitmap.contains("set_write_intercept(IA32_XSS_MSR, true)"));

    let builtin = section(SVM_VCPU, "fn builtin_vmexit_handler", "fn handle_cr_write");
    assert!(builtin.contains("IA32_XSS_MSR"));
    assert!(builtin.contains("handle_xss_msr_access"));
}

#[test]
fn svm_guest_and_host_extended_register_contents_have_distinct_owners() {
    let switch = section(
        SVM_VCPU,
        "unsafe extern \"C\" fn svm_world_switch",
        "/// Host save area used to restore CPU state touched by SVM VMLOAD/VMSAVE.",
    );
    assert!(switch.contains("install_guest_xstate_from_rdi!()"));
    assert!(switch.contains("restore_host_xstate_from_rdi!()"));

    let cpuid = section(SVM_VCPU, "fn handle_cpuid", "fn handle_xsetbv");
    assert!(
        !cpuid.contains("cpuid_with_guest_state"),
        "CPUID(D) must be synthesized without changing live host xstate ownership"
    );
}

#[test]
fn every_svm_bind_and_run_revalidates_the_current_cpu_xstate_contract() {
    let run = section(SVM_VCPU, "pub fn run", "pub fn bind");
    assert!(run.contains("self.world_switch.xstate.validate_current_cpu(cpu_pin)?"));
    let bind = section(SVM_VCPU, "pub fn bind", "pub fn unbind");
    assert!(bind.contains("self.world_switch.xstate.validate_current_cpu(cpu_pin)?"));
}

#[test]
fn invalid_svm_xsetbv_injects_gp_without_returning_a_host_error() {
    let handler = section(SVM_VCPU, "fn handle_xsetbv", "fn svm_io_exit_info");
    assert!(handler.contains("queue_event(GENERAL_PROTECTION_VECTOR, Some(0))"));
    assert!(!handler.contains("ok_or(x86_err_type!(InvalidInput))"));
    assert!(!handler.contains("x86_err!(Unsupported"));
}

#[test]
fn svm_cpuid_osxsave_follows_guest_cr4() {
    let cpuid = section(SVM_VCPU, "fn handle_cpuid", "fn handle_xsetbv");
    assert!(cpuid.contains("guest_osxsave"));
}
