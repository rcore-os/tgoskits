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
const XSTATE: &str = include_str!("../src/xstate.rs");

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
        "fn bind_to_current_processor",
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

#[test]
fn guest_xstate_is_confined_to_the_irq_off_world_switch_window() {
    let run = section(VMX, "fn inner_run", "fn setup_vmcs");
    assert_in_order(
        run,
        "capture_and_disable_host_interrupts",
        "self.xstate.capture_host()",
    );
    assert_in_order(run, "self.xstate.capture_host()", "self.vmx_launch()");
    assert_in_order(run, "self.vmx_launch()", "restore_host_interrupt_flag");

    let entry = section(
        VMX,
        "macro_rules! vmx_entry_with",
        "impl<H: X86HostOps> VmxVcpu<H>",
    );
    assert!(
        !entry.contains("pushfq"),
        "VM entry must preserve the flags captured before guest xstate became live"
    );
    assert!(
        !entry.contains("host_rflags ="),
        "VM entry must not overwrite the pre-xstate host interrupt flags"
    );
    assert_in_order(
        entry,
        "install_guest_xstate_from_rdi!()",
        "restore_regs_from_stack!()",
    );

    let exit = section(
        VMX,
        "unsafe extern \"C\" fn vmx_exit",
        "fn vmx_entry_failed",
    );
    assert_in_order(
        exit,
        "save_regs_to_stack!()",
        "restore_host_xstate_from_rdi!()",
    );
    assert_in_order(
        exit,
        "restore_host_xstate_from_rdi!()",
        "restore_regs_from_stack!()",
    );

    assert!(XSTATE.contains("macro_rules! install_guest_xstate_from_rdi"));
    assert!(XSTATE.contains("macro_rules! restore_host_xstate_from_rdi"));
    assert!(XSTATE.contains("xsetbv"));
    assert!(XSTATE.contains("xgetbv"));
}

#[test]
fn guest_and_host_extended_register_contents_have_distinct_owners() {
    assert!(
        XSTATE.contains("host_area") && XSTATE.contains("guest_area"),
        "the vCPU must keep separate host scratch and persistent guest xstate areas"
    );
    assert!(
        XSTATE.contains("xsave64") && XSTATE.contains("xrstor64"),
        "the world switch must transfer xstate register contents as well as XCR0"
    );

    let entry = section(
        VMX,
        "macro_rules! vmx_entry_with",
        "impl<H: X86HostOps> VmxVcpu<H>",
    );
    assert!(entry.contains("install_guest_xstate_from_rdi!()"));

    let exit = section(
        VMX,
        "unsafe extern \"C\" fn vmx_exit",
        "fn vmx_entry_failed",
    );
    assert!(exit.contains("restore_host_xstate_from_rdi!()"));
}

#[test]
fn cpuid_policy_does_not_temporarily_install_guest_xcr0() {
    let cpuid = section(VMX, "fn handle_cpuid", "fn handle_xsetbv");
    assert!(
        !cpuid.contains("cpuid_with_guest_state"),
        "CPUID(D) must be synthesized without changing live host xstate ownership"
    );
    assert!(
        XSTATE.contains("guest_xstate_size"),
        "CPUID(D).0 EBX must be derived from the guest XCR0 policy"
    );
}

#[test]
fn every_vmx_bind_and_run_revalidates_the_current_cpu_xstate_contract() {
    assert!(XSTATE.contains("struct XStateContract"));
    assert!(XSTATE.contains("fn validate_current_cpu"));
    assert!(
        XSTATE.contains("host_xss != 0"),
        "the backend must reject supervisor state until it owns an XSAVES area"
    );

    let run = section(VMX, "pub fn run", "pub fn bind");
    assert!(run.contains("self.xstate.validate_current_cpu(cpu_pin)?"));
    let bind = section(VMX, "pub fn bind", "pub fn unbind");
    assert!(bind.contains("self.xstate.validate_current_cpu(cpu_pin)?"));
}

#[test]
fn invalid_vmx_xsetbv_injects_gp_without_panicking_the_host() {
    let handler = section(VMX, "fn handle_xsetbv", "impl<H: X86HostOps> Drop");
    assert!(handler.contains("queue_event(GENERAL_PROTECTION_VECTOR, Some(0))"));
    assert!(!handler.contains("ok_or(x86_err_type!(InvalidInput))"));
    assert!(!handler.contains("x86_err!(Unsupported"));
}

#[test]
fn vmx_cpuid_osxsave_follows_guest_cr4() {
    let cpuid = section(VMX, "fn handle_cpuid", "fn handle_xsetbv");
    assert!(cpuid.contains("guest_osxsave"));
    assert!(XSTATE.contains("CPUID_FEATURE_OSXSAVE"));
}

#[test]
fn guest_xsetbv_is_bounded_by_the_kernel_managed_mask() {
    let handler = section(VMX, "fn handle_xsetbv", "impl<H: X86HostOps> Drop");
    assert!(
        handler.contains("validate_guest_xsetbv"),
        "both backends must use the shared CPUID/XCR0 capability closure"
    );
    assert!(handler.contains("INVALID_OPCODE_VECTOR"));
    assert!(XSTATE.contains("supports_guest_xcr0"));
    assert!(XSTATE.contains("managed_xcr0"));
    assert!(XSTATE.contains("standard_size"));
    assert!(!XSTATE.contains("supported_xcr0"));
}

#[test]
fn vmx_guest_cpuid_and_xss_share_the_managed_xstate_policy() {
    let cpuid = section(VMX, "fn handle_cpuid", "fn handle_xsetbv");
    assert!(
        cpuid.contains("filter_guest_cpuid"),
        "all xstate-dependent feature leaves must use the shared policy"
    );

    let bitmap = section(VMX, "fn setup_msr_bitmap", "fn setup_vmcs");
    assert!(bitmap.contains("set_read_intercept(IA32_XSS_MSR, true)"));
    assert!(bitmap.contains("set_write_intercept(IA32_XSS_MSR, true)"));

    let builtin = section(
        VMX,
        "fn builtin_vmexit_handler",
        "/// Read a 64-bit value from EDX:EAX.",
    );
    assert!(builtin.contains("IA32_XSS_MSR"));
    assert!(builtin.contains("handle_xss_msr_access"));

    let controls = section(VMX, "fn setup_vmcs_control", "fn get_paging_level");
    assert!(
        !controls.contains("ENABLE_XSAVES_XRSTORS"),
        "XSAVES/XRSTORS must stay disabled while guest supervisor xstate is hidden"
    );
}
