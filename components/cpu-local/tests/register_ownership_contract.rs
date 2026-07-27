//! Architecture register ownership and assembly allowlist contract.

use std::{fs, mem::MaybeUninit, path::Path};

#[test]
#[cfg(feature = "host-test")]
fn each_host_thread_must_install_its_own_cpu_binding() {
    std::thread::spawn(|| {
        assert_eq!(
            unsafe { cpu_local::with_cpu_pin(|_| ()) },
            Err(cpu_local::CpuLocalError::AreaNotInstalled)
        );

        let storage = Box::leak(Box::new(MaybeUninit::<cpu_local::CpuAreaPrefix>::uninit()));
        let base = storage.as_mut_ptr() as usize;
        storage.write(
            cpu_local::CpuAreaPrefix::initialize(cpu_local::CpuIndex::try_from(1).unwrap(), base)
                .unwrap(),
        );
        let area = unsafe { cpu_local::CpuAreaRef::from_initialized_base(base) }.unwrap();
        // SAFETY: this thread explicitly owns the leaked CPU fixture and cannot
        // receive modeled traps while installing the completed area.
        unsafe { cpu_local::install_cpu_area(area) }.unwrap();
        assert_eq!(
            unsafe { cpu_local::with_cpu_pin(|pin| pin.area().base()) },
            Ok(base)
        );
    })
    .join()
    .unwrap();
}

const REGISTER: &str = include_str!("../src/register/mod.rs");
const X86_64: &str = include_str!("../src/register/x86_64.rs");
const AARCH64: &str = include_str!("../src/register/aarch64.rs");
const RISCV: &str = include_str!("../src/register/riscv.rs");
const LOONGARCH64: &str = include_str!("../src/register/loongarch64.rs");
const IDENTITY: &str = include_str!("../src/identity.rs");
const SYMBOL: &str = include_str!("../src/symbol.rs");

#[test]
fn architecture_assembly_stays_in_the_leaf_allowlist() {
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let allowed = ["x86_64.rs", "aarch64.rs", "riscv.rs", "loongarch64.rs"];

    for path in rust_sources(&source_dir) {
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
fn register_validation_returns_a_typed_cpu_area() {
    assert!(
        REGISTER.contains("fn current_area() -> Result<CpuAreaRef, CpuLocalError>"),
        "register validation must reconstruct a typed CpuAreaRef"
    );
}

#[test]
fn register_leaf_does_not_export_raw_area_or_thread_access() {
    assert!(
        !REGISTER.contains("pub unsafe fn current_area_base_raw(")
            && !REGISTER.contains("pub unsafe fn current_thread_raw("),
        "architecture values must remain behind shared typed validation"
    );
    assert!(
        !REGISTER.contains("pub fn current_area_base(")
            && !REGISTER.contains("pub fn current_header(")
            && !REGISTER.contains("pub fn verify_current("),
        "the leaf must not expose safe access that bypasses CpuPin"
    );
}

#[test]
fn template_metadata_uses_position_independent_rust_addresses() {
    assert!(
        !SYMBOL.contains("asm!(") && !SYMBOL.contains("target_arch"),
        "template metadata must not embed architecture-specific absolute relocations"
    );
    for symbol in ["__CPU_LOCAL_AREA_PREFIX", "__CPU_LOCAL_TEMPLATE_END"] {
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
    let backend = RISCV;
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
    let x86 = X86_64;
    assert!(x86.contains("IA32_GS_BASE"));
    assert!(x86.contains("gs:[{offset}]") && x86.contains("CPU_AREA_SELF_BASE_OFFSET"));
    assert!(x86.contains("CPU_AREA_CURRENT_THREAD_OFFSET"));
    let x86_install = x86
        .split_once("unsafe fn install_cpu_base")
        .unwrap()
        .1
        .split_once("unsafe fn read_cpu_base")
        .unwrap()
        .0;
    assert!(!x86_install.contains("IA32_FS_BASE"));

    let aarch64 = AARCH64;
    assert!(aarch64.contains("TPIDR_EL1"));
    assert!(aarch64.contains("TPIDR_EL2"));
    let install = aarch64
        .split_once("unsafe fn install_cpu_base")
        .unwrap()
        .1
        .split_once("unsafe fn read_cpu_base")
        .unwrap()
        .0;
    let area_read = aarch64
        .split_once("unsafe fn read_cpu_base")
        .unwrap()
        .1
        .split_once("unsafe fn read_current_thread")
        .unwrap()
        .0;
    assert!(!install.contains("TPIDR_EL0"));
    assert!(!area_read.contains("TPIDR_EL0"));
    let task_pointer = aarch64.split_once("unsafe fn read_kernel_tls").unwrap().1;
    assert!(task_pointer.contains("TPIDR_EL0"));
    assert!(aarch64.contains("cfg(feature = \"tls\")"));
}

#[test]
fn loongarch_mirrors_the_direct_area_base_in_ks3() {
    let backend = LOONGARCH64;
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

fn words(source: &str) -> impl Iterator<Item = &str> {
    source
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|word| !word.is_empty())
}

fn rust_sources(root: &Path) -> Vec<std::path::PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut sources = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("cpu-local source directory must be readable")
        {
            let path = entry.expect("source entry must be readable").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                sources.push(path);
            }
        }
    }
    sources
}
