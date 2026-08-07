const VMX_VCPU: &str = include_str!("vmx/vcpu.rs");
const SVM_VCPU: &str = include_str!("svm/vcpu.rs");

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing section start {start:?}"))
        .1
        .split_once(end)
        .unwrap_or_else(|| panic!("missing section end {end:?}"))
        .0
}

fn assert_in_order(source: &str, operations: &[&str]) {
    let mut cursor = 0;
    for operation in operations {
        let offset = source[cursor..]
            .find(operation)
            .unwrap_or_else(|| panic!("missing ordered operation {operation:?}"));
        cursor += offset + operation.len();
    }
}

#[test]
fn vmx_refreshes_host_tls_before_guest_entry_and_returns_without_rust_calls() {
    let bind = section(
        VMX_VCPU,
        "pub fn bind_to_current_processor",
        "/// Unbind this [`VmxVcpu`]",
    );
    assert_in_order(bind, &["vmx::vmptrld", "self.setup_vmcs_host()?"]);

    let host = section(VMX_VCPU, "fn setup_vmcs_host", "fn setup_vmcs_guest");
    for register in [
        "VmcsHostNW::FS_BASE.write(Msr::IA32_FS_BASE.read() as _)?",
        "VmcsHostNW::GS_BASE.write(Msr::IA32_GS_BASE.read() as _)?",
        "VmcsHostNW::RIP.write(Self::vmx_exit as *const () as usize)?",
    ] {
        assert!(
            host.contains(register),
            "missing host register {register:?}"
        );
    }

    let exit = section(
        VMX_VCPU,
        "unsafe extern \"C\" fn vmx_exit",
        "fn vmx_entry_failed",
    );
    assert_in_order(
        exit,
        &[
            "save_regs_to_stack!()",
            "restore_regs_from_stack!()",
            "\"ret\"",
        ],
    );
    assert!(!exit.contains("\"call "));
}

#[test]
fn svm_restores_host_tls_inside_the_naked_world_switch() {
    let switch = section(
        SVM_VCPU,
        "unsafe extern \"C\" fn svm_world_switch",
        "/// Host save area used to restore CPU state touched by SVM VMLOAD/VMSAVE.",
    );
    assert!(switch.contains("naked_asm!"));
    assert_in_order(
        switch,
        &["vmload rax", "vmrun rax", "vmsave rax", "vmload rax", "ret"],
    );
    assert_eq!(switch.match_indices("vmload rax").count(), 2);
    assert!(!switch.contains("\"call "));

    let prepare = section(SVM_VCPU, "fn prepare_world_switch", "pub unsafe fn svm_run");
    assert!(!prepare.contains("instructions::vmload"));
    let run = section(SVM_VCPU, "pub unsafe fn svm_run", "\n    }\n}");
    assert!(run.contains("svm_world_switch("));
    assert!(!run.contains("instructions::vmload"));
    assert!(!run.contains("instructions::vmsave"));
}
