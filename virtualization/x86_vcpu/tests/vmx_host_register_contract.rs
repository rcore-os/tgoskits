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

//! Source-level ownership contract for VMX host GS/FS restoration.
//!
//! VM-exit restores VMCS host state in hardware. Since a vCPU can migrate,
//! every CPU bind must refresh the VMCS host FS and GS bases before entry.

const VMX: &str = include_str!("../src/vmx/vcpu.rs");

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing `{start}`"))
        .1
        .split_once(end)
        .unwrap_or_else(|| panic!("missing `{end}` after `{start}`"))
        .0
}

fn assert_in_order(source: &str, earlier: &str, later: &str) {
    let earlier = source
        .find(earlier)
        .unwrap_or_else(|| panic!("missing `{earlier}`"));
    let later = source
        .find(later)
        .unwrap_or_else(|| panic!("missing `{later}`"));
    assert!(earlier < later, "`{earlier}` must precede `{later}`");
}

#[test]
fn every_vmx_cpu_bind_refreshes_host_fs_and_gs() {
    let bind = section(
        VMX,
        "pub fn bind_to_current_processor",
        "/// Unbind this [`VmxVcpu`]",
    );
    assert_in_order(bind, "vmx::vmptrld", "self.setup_vmcs_host()?");

    let host = section(VMX, "fn setup_vmcs_host", "fn setup_vmcs_guest");
    assert!(host.contains("VmcsHostNW::FS_BASE.write(Msr::IA32_FS_BASE.read() as _)?"));
    assert!(host.contains("VmcsHostNW::GS_BASE.write(Msr::IA32_GS_BASE.read() as _)?"));
    assert!(host.contains("VmcsHostNW::RIP.write(Self::vmx_exit as *const () as usize)?"));

    let public_bind = section(VMX, "pub fn bind(&mut self", "pub fn unbind(&mut self");
    assert!(
        public_bind.contains("self.bind_to_current_processor()"),
        "the pinned backend bind must refresh the VMCS host register image"
    );
}

#[test]
fn vm_exit_returns_only_after_hardware_installs_host_registers() {
    let exit = section(
        VMX,
        "unsafe extern \"C\" fn vmx_exit",
        "fn vmx_entry_failed",
    );
    assert!(exit.contains("naked_asm!("));
    assert_in_order(exit, "save_regs_to_stack!()", "restore_regs_from_stack!()");
    assert_in_order(exit, "restore_regs_from_stack!()", "\"ret\"");
    assert!(
        !exit.contains("\"call "),
        "the VM-exit trampoline must not call Rust or helpers"
    );
}
