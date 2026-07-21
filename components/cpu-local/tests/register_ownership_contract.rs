//! Architecture register ownership and assembly allowlist contract.

use std::{fs, path::Path};

#[test]
#[cfg(feature = "host-test")]
fn each_host_thread_must_install_its_own_cpu_binding() {
    std::thread::spawn(|| {
        // SAFETY: this fixture thread models one non-migrating CPU.
        let pin = unsafe { cpu_local::CpuPin::new_unchecked() };
        assert_eq!(
            cpu_local::raw::current_binding(&pin),
            Err(cpu_local::CpuLocalError::NotInitialized)
        );

        let prefix = Box::leak(Box::new(cpu_local::CpuAreaPrefix::template()));
        let base = (prefix as *mut cpu_local::CpuAreaPrefix) as usize;
        *prefix = cpu_local::CpuAreaPrefix::for_area(
            cpu_local::CpuIndex::try_from(1).unwrap(),
            base,
            1,
            0xace0,
        );
        // SAFETY: this thread explicitly owns the leaked CPU fixture and cannot
        // receive modeled traps while installing the complete frozen binding.
        unsafe { cpu_local::raw::install_binding(prefix.header().binding()) }.unwrap();
        assert_eq!(unsafe { cpu_local::raw::current_area_base_raw(&pin) }, base);
    })
    .join()
    .unwrap();
}

const REGISTER: &str = include_str!("../src/register.rs");
const IDENTITY: &str = include_str!("../src/identity.rs");
const SYMBOL: &str = include_str!("../src/symbol.rs");

#[test]
fn architecture_assembly_stays_in_the_leaf_allowlist() {
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let allowed = ["register.rs"];

    for entry in fs::read_dir(source_dir).expect("cpu-local source directory must be readable") {
        let path = entry.expect("source entry must be readable").path();
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        assert!(
            !source.contains("global_asm!"),
            "{} must not contain global assembly",
            path.display()
        );
        if source.contains("asm!") {
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("Rust source name must be UTF-8");
            assert!(
                allowed.contains(&file_name),
                "architecture assembly must stay in the leaf allowlist, found {}",
                path.display()
            );
        }
    }
}

#[test]
fn unchecked_area_base_uses_a_nonnull_pointer_capability() {
    assert!(
        REGISTER.contains("pub unsafe fn current_area_base_unchecked() -> NonNull<u8>"),
        "the unchecked boot/trap boundary must not expose an untyped nullable integer base"
    );
}

#[test]
fn leaf_exports_only_raw_value_reads_for_higher_layer_verification() {
    assert!(
        REGISTER.contains("pub unsafe fn current_area_base_raw("),
        "ax-percpu needs a raw value-only register read before layout verification"
    );
    assert!(
        !REGISTER.contains("pub fn current_area_base(")
            && !REGISTER.contains("pub fn current_header(")
            && !REGISTER.contains("pub fn verify_current("),
        "the leaf must not expose safe current-area access that bypasses BoundCpuPin"
    );
    assert!(
        !REGISTER.contains("read_current_area_base() -> CpuLocalAnchor"),
        "raw register observation must not dereference a header to recover relocation"
    );
}

#[test]
fn template_metadata_uses_position_independent_rust_addresses() {
    assert!(
        !SYMBOL.contains("asm!(") && !SYMBOL.contains("target_arch"),
        "template metadata must not embed architecture-specific absolute relocations"
    );
    for symbol in ["__AX_CPU_AREA_PREFIX", "__AX_CPU_AREA_TEMPLATE_END"] {
        assert!(
            SYMBOL.contains(&format!("addr_of!(crate::{symbol})")),
            "rustc must materialize the load-relocated address of {symbol}"
        );
    }
    assert!(
        SYMBOL.contains("checked_sub") && SYMBOL.contains("checked_add"),
        "template size must be derived from a checked relative range"
    );
}

#[test]
fn riscv_uses_linux_current_or_unikernel_scratch_by_image_mode() {
    let backend = architecture_backend(REGISTER, "riscv32", "loongarch64");
    assert!(backend.contains("csrw sscratch, zero"));
    assert!(backend.contains("csrw sscratch, {base}"));
    assert!(backend.contains("mv tp, {current}"));
    assert!(backend.contains("csrr {base}, sscratch"));
    for register in words(backend) {
        assert!(
            register != "gp",
            "RISC-V CPU-local code must preserve the psABI global pointer"
        );
    }
}

#[test]
fn x86_and_aarch64_keep_task_tls_separate_from_the_cpu_anchor() {
    let x86 = architecture_backend(REGISTER, "x86_64", "aarch64");
    assert!(x86.contains("IA32_GS_BASE"));
    assert!(x86.contains("gs:[{self_base_offset}]"));
    assert!(x86.contains("gs:[{current_thread_offset}]"));
    let x86_install = x86
        .split_once("pub unsafe fn install_current")
        .unwrap()
        .1
        .split_once("pub unsafe fn read_current_area_base")
        .unwrap()
        .0;
    assert!(!x86_install.contains("IA32_FS_BASE"));

    let aarch64 = architecture_backend(REGISTER, "aarch64", "riscv32");
    assert!(aarch64.contains("TPIDR_EL1"));
    assert!(aarch64.contains("TPIDR_EL2"));
    let install = aarch64
        .split_once("pub unsafe fn install_current")
        .unwrap()
        .1
        .split_once("pub unsafe fn read_current_area_base")
        .unwrap()
        .0;
    let area_read = aarch64
        .split_once("pub unsafe fn read_current_area_base")
        .unwrap()
        .1
        .split_once("pub unsafe fn read_current_thread")
        .unwrap()
        .0;
    assert!(!install.contains("TPIDR_EL0"));
    assert!(!area_read.contains("TPIDR_EL0"));
    let task_pointer = aarch64
        .split_once("pub unsafe fn get_task_pointer")
        .unwrap()
        .1;
    assert!(task_pointer.contains("TPIDR_EL0"));
    assert!(task_pointer.contains("RegisterModeV1::LinuxCurrent"));
}

#[test]
fn loongarch_mirrors_the_direct_area_base_in_ks3() {
    let backend = architecture_backend(REGISTER, "loongarch64", "arm");
    assert!(backend.contains("csrwr {shadow}, 0x33"));
    assert!(backend.contains("move $r21, {base}"));
    assert!(backend.contains("csrrd {shadow}, 0x33"));
    assert!(
        !backend.contains("cpu_area_header_link_address") && !backend.contains("PerCpuRelocation"),
        "the CPU-owned LoongArch anchor must not depend on the kernel image address"
    );

    assert!(
        !IDENTITY.contains("CpuLocalAnchor") && !IDENTITY.contains("relocation"),
        "single-word anchor values must not bypass the complete frozen binding"
    );
}

fn architecture_backend<'source>(source: &'source str, start: &str, end: &str) -> &'source str {
    source
        .split_once(&format!("target_arch = \"{start}\""))
        .unwrap_or_else(|| panic!("missing {start} register backend"))
        .1
        .split_once(&format!("target_arch = \"{end}\""))
        .unwrap_or_else(|| panic!("missing end marker after {start} register backend"))
        .0
}

fn words(source: &str) -> impl Iterator<Item = &str> {
    source
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|word| !word.is_empty())
}
