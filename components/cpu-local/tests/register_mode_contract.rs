const MANIFEST: &str = include_str!("../Cargo.toml");
const HEADER: &str = concat!(
    include_str!("../src/lib.rs"),
    include_str!("../src/area.rs"),
    include_str!("../src/thread.rs"),
);
const REGISTER: &str = concat!(
    include_str!("../src/register/mod.rs"),
    include_str!("../src/register/x86_64.rs"),
    include_str!("../src/register/aarch64.rs"),
    include_str!("../src/register/riscv.rs"),
    include_str!("../src/register/loongarch64.rs"),
);
const X86_64: &str = include_str!("../src/register/x86_64.rs");
const AARCH64: &str = include_str!("../src/register/aarch64.rs");
const RISCV: &str = include_str!("../src/register/riscv.rs");
const LOONGARCH64: &str = include_str!("../src/register/loongarch64.rs");
const SYMBOL: &str = include_str!("../src/symbol.rs");

#[test]
fn image_mode_is_additive_but_the_prefix_layout_is_not() {
    assert!(
        MANIFEST.contains("tls = []"),
        "cpu-local must expose the final-image UnikernelTls selector"
    );
    assert!(
        !MANIFEST.contains("arm-el2"),
        "the CPU-local leaf must discover the live AArch64 exception level at runtime"
    );
    assert!(
        HEADER.contains("pub struct CpuAreaPrefix")
            && HEADER.contains("pub struct CpuRuntimeAnchor")
            && HEADER.contains("pub struct BootContextHeader")
            && HEADER.contains("pub struct ExecutionContextHeader"),
        "the prefix must reserve runtime-anchor and boot-thread cache lines"
    );
    assert!(
        HEADER.contains("CPU_AREA_RUNTIME_ANCHOR_OFFSET")
            && HEADER.contains("CPU_AREA_BOOT_CONTEXT_OFFSET")
            && HEADER.contains("size_of::<CpuAreaPrefix>() == 192"),
        "the prefix must keep runtime state at 64 and the boot header at 128"
    );
    for type_name in [
        "CpuRuntimeAnchor",
        "CpuAreaPrefix",
        "ExecutionContextHeader",
    ] {
        let definition = HEADER
            .split_once(&format!("pub struct {type_name}"))
            .unwrap_or_else(|| panic!("{type_name} must exist"))
            .1
            .split_once("\n}")
            .unwrap_or_else(|| panic!("{type_name} must have a bounded definition"))
            .0;
        assert!(
            !definition.contains("cfg(feature = \"tls\")"),
            "Cargo image mode must never alter {type_name} layout"
        );
    }
}

#[test]
fn execution_context_header_is_scheduler_neutral_and_resource_free() {
    let header = HEADER
        .split_once("pub struct ExecutionContextHeader")
        .expect("ExecutionContextHeader must exist")
        .1
        .split_once("\n}")
        .expect("ExecutionContextHeader must have a bounded definition")
        .0;

    for field in [
        "cpu_area",
        "binding_epoch",
        "architecture_state",
        "preemption_state",
    ] {
        assert!(
            header.contains(field),
            "ExecutionContextHeader is missing {field}"
        );
    }
    for forbidden in [
        "CurrentContext",
        "RuntimeThreadCookie",
        "kernel_tls",
        "stack",
        "TaskContext",
        "address_space",
    ] {
        assert!(
            !header.contains(forbidden),
            "ExecutionContextHeader must not own {forbidden}"
        );
    }

    for api in [
        "pub const fn new(",
        "unsafe fn bind_cpu(",
        "fn cpu_binding(",
    ] {
        assert!(
            HEADER.contains(api),
            "ExecutionContextHeader is missing `{api}`"
        );
    }
}

#[test]
fn cpu_local_has_no_upper_layer_dependency_or_scheduler_vocabulary() {
    for forbidden in ["ax-task", "ax-runtime"] {
        assert!(
            !MANIFEST.contains(forbidden),
            "cpu-local must not depend on {forbidden}"
        );
    }
    for forbidden in ["scheduler_", "RuntimeThreadCookie"] {
        assert!(
            !HEADER.contains(forbidden) && !REGISTER.contains(forbidden),
            "cpu-local public mechanism still exposes {forbidden}"
        );
    }
}

#[test]
fn each_image_mode_selects_one_current_context_source() {
    for (backend, linux, tls) in [
        (X86_64, "RuntimeAnchor", "RuntimeAnchor"),
        (AARCH64, "ArchitectureRegister", "ArchitectureRegister"),
        (RISCV, "ArchitectureRegister", "RuntimeAnchor"),
        (LOONGARCH64, "ArchitectureRegister", "RuntimeAnchor"),
    ] {
        assert!(backend.contains(&format!("linux_current: CurrentContextSource::{linux}")));
        assert!(backend.contains(&format!("unikernel_tls: CurrentContextSource::{tls}")));
    }
}

#[test]
fn register_backends_implement_both_compile_time_image_modes() {
    assert!(REGISTER.contains("cfg(feature = \"tls\")"));

    assert!(X86_64.contains("IA32_GS_BASE"));

    for register in ["CurrentEL", "TPIDR_EL1", "TPIDR_EL2", "SP_EL0"] {
        assert!(
            AARCH64.contains(register),
            "AArch64 dual-mode binding is missing {register}"
        );
    }

    assert!(
        RISCV.contains("csrw sscratch, zero") && RISCV.contains("mv tp"),
        "RISC-V LinuxCurrent must install tp=current header and leave kernel sscratch zero"
    );
    assert!(
        RISCV.contains("csrw sscratch, {base}"),
        "RISC-V UnikernelTls must retain the CPU prefix in sscratch"
    );

    for operation in ["move $r21", "0x33", "move $tp"] {
        assert!(
            LOONGARCH64.contains(operation),
            "LoongArch binding is missing {operation}"
        );
    }
}

#[test]
fn riscv_template_symbols_never_use_absolute_relocation_assembly() {
    for forbidden in ["%highest", "%higher", "%hi(", "%lo(", "global_asm!"] {
        assert!(
            !SYMBOL.contains(forbidden),
            "position-independent template metadata contains forbidden `{forbidden}`"
        );
    }
    assert!(!SYMBOL.contains("asm!("));
}
