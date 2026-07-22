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

    let prepare = section(SVM_VCPU, "fn prepare_world_switch", "pub unsafe fn svm_run");
    assert!(
        !prepare.contains("instructions::vmload"),
        "preparation must not install guest FS/GS before entering assembly"
    );

    let run = section(SVM_VCPU, "pub unsafe fn svm_run", "\n    }\n}");
    assert!(run.contains("svm_world_switch("));
    assert!(!run.contains("instructions::vmload"));
    assert!(!run.contains("instructions::vmsave"));
}
