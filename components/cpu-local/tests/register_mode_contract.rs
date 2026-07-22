const MANIFEST: &str = include_str!("../Cargo.toml");
const HEADER: &str = concat!(
    include_str!("../src/header/mod.rs"),
    include_str!("../src/header/area.rs"),
    include_str!("../src/header/thread.rs"),
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
        HEADER.contains("pub struct CpuAreaPrefixV2")
            && HEADER.contains("pub struct CpuRuntimeAnchor")
            && HEADER.contains("pub struct BootThreadHeader")
            && HEADER.contains("pub struct CurrentThreadHeader"),
        "the stable v2 prefix must reserve runtime-anchor and boot-thread cache lines"
    );
    assert!(
        HEADER.contains("CPU_AREA_RUNTIME_ANCHOR_OFFSET")
            && HEADER.contains("CPU_AREA_BOOT_THREAD_OFFSET")
            && HEADER.contains("size_of::<CpuAreaPrefixV2>() == 192"),
        "the v2 prefix ABI must keep runtime state at 64 and the boot header at 128"
    );
    assert!(
        !HEADER.contains("cfg(feature = \"tls\")"),
        "Cargo image mode must never alter CpuAreaPrefixV2 or CurrentThreadHeader layout"
    );
}

#[test]
fn current_thread_header_is_task_owned_and_resource_free() {
    let header = HEADER
        .split_once("pub struct CurrentThreadHeader")
        .expect("CurrentThreadHeader must exist")
        .1
        .split_once("\n}")
        .expect("CurrentThreadHeader must have a bounded definition")
        .0;

    for field in [
        "thread_identity",
        "context_identity",
        "cpu_base",
        "cpu_index",
        "binding_epoch",
    ] {
        assert!(
            header.contains(field),
            "CurrentThreadHeader is missing {field}"
        );
    }
    for forbidden in ["kernel_tls", "stack", "TaskContext", "address_space"] {
        assert!(
            !header.contains(forbidden),
            "CurrentThreadHeader must not own {forbidden}"
        );
    }

    for api in [
        "pub const fn new(",
        "pub fn bind_thread(",
        "pub unsafe fn bind_cpu(",
        "pub fn cpu_binding(",
    ] {
        assert!(
            HEADER.contains(api),
            "CurrentThreadHeader is missing `{api}`"
        );
    }
}

#[test]
fn register_backends_implement_both_image_modes() {
    for mode in ["LinuxCurrent", "UnikernelTls"] {
        assert!(
            REGISTER.contains(mode),
            "architecture register binding is missing the {mode} mode"
        );
    }

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
