//! Architecture register ownership and assembly allowlist contract.

use std::{fs, path::Path};

#[test]
#[cfg(feature = "host-test")]
fn host_threads_inherit_the_bootstrap_fixture_anchor_until_overridden() {
    let bootstrap_prefix = Box::leak(Box::new(ax_cpu_local::CpuAreaPrefix::TEMPLATE));
    let bootstrap_base = (bootstrap_prefix as *mut ax_cpu_local::CpuAreaPrefix) as usize;
    let bootstrap_anchor = ax_cpu_local::CpuLocalAnchor::new(
        bootstrap_base,
        ax_cpu_local::PerCpuRelocation::from_raw(0),
    );
    *bootstrap_prefix = ax_cpu_local::CpuAreaPrefix::for_area(
        ax_cpu_local::CpuIndex::try_from(0).unwrap(),
        bootstrap_anchor,
        1,
        0xace0,
    );
    // SAFETY: the leaked prefix remains mapped for the process lifetime, and
    // this host fixture cannot receive architecture traps.
    unsafe { ax_cpu_local::install_current(bootstrap_anchor) };

    let inherited_base = std::thread::spawn(move || {
        let inherited_base = {
            // SAFETY: this fixture thread cannot migrate while it observes the
            // inherited bootstrap anchor.
            let pin = unsafe { ax_cpu_local::CpuPin::new_unchecked() };
            // SAFETY: the fixture anchor is mapped and this thread remains on
            // the modeled CPU for the complete raw register read.
            unsafe { ax_cpu_local::current_area_base_raw(&pin) }
        };

        let override_prefix = Box::leak(Box::new(ax_cpu_local::CpuAreaPrefix::TEMPLATE));
        let override_base = (override_prefix as *mut ax_cpu_local::CpuAreaPrefix) as usize;
        let override_anchor = ax_cpu_local::CpuLocalAnchor::new(
            override_base,
            ax_cpu_local::PerCpuRelocation::from_raw(0),
        );
        *override_prefix = ax_cpu_local::CpuAreaPrefix::for_area(
            ax_cpu_local::CpuIndex::try_from(1).unwrap(),
            override_anchor,
            1,
            0xace0,
        );
        // SAFETY: the leaked override remains live for the process, and this
        // fixture thread is pinned to the modeled CPU for the following read.
        unsafe { ax_cpu_local::install_current(override_anchor) };

        // SAFETY: the fixture thread cannot migrate after its explicit
        // thread-local installation.
        let override_pin = unsafe { ax_cpu_local::CpuPin::new_unchecked() };
        assert_eq!(
            unsafe { ax_cpu_local::current_area_base_raw(&override_pin) },
            override_base
        );
        inherited_base
    })
    .join()
    .unwrap();

    assert_eq!(inherited_base, bootstrap_base);
}

const REGISTER: &str = include_str!("../src/register.rs");
const SYMBOL: &str = include_str!("../src/symbol.rs");

#[test]
fn architecture_assembly_stays_in_the_leaf_allowlist() {
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let allowed = ["register.rs", "symbol.rs"];

    for entry in fs::read_dir(source_dir).expect("ax-cpu-local source directory must be readable") {
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
fn link_address_materialization_preserves_the_full_pointer_width() {
    assert!(
        SYMBOL.contains("mov {address}, offset {prefix}")
            && !SYMBOL.contains("mov {address:e}, offset {prefix}"),
        "x86_64 must not zero-extend a 32-bit link address"
    );
    for relocation in ["abs_g0_nc", "abs_g1_nc", "abs_g2_nc", "abs_g3"] {
        assert!(
            SYMBOL.contains(relocation),
            "AArch64 link address materialization is missing {relocation}"
        );
    }
    for relocation in ["%highest", "%higher"] {
        assert!(
            SYMBOL.contains(relocation),
            "RISC-V link address materialization is missing {relocation}"
        );
    }
    for instruction in ["lu32i.d", "lu52i.d"] {
        assert!(
            SYMBOL.contains(instruction),
            "LoongArch link address materialization is missing {instruction}"
        );
    }
}

#[test]
fn riscv_uses_only_the_scratch_anchor() {
    let backend = architecture_backend("riscv32", "loongarch64");
    assert!(backend.contains("csrw sscratch"));
    assert!(backend.contains("csrr {base}, sscratch"));
    for register in words(backend) {
        assert!(
            !matches!(register, "gp" | "tp"),
            "RISC-V CPU-local code must not borrow task/global register {register}"
        );
    }
}

#[test]
fn x86_and_aarch64_keep_task_tls_separate_from_the_cpu_anchor() {
    let x86 = architecture_backend("x86_64", "aarch64");
    assert!(x86.contains("IA32_GS_BASE"));
    assert!(x86.contains("rdmsr"));
    assert!(!x86.contains("gs:["));

    let aarch64 = architecture_backend("aarch64", "riscv32");
    assert!(aarch64.contains("TPIDR_EL1"));
    assert!(aarch64.contains("TPIDR_EL2"));
    assert!(!aarch64.contains("TPIDR_EL0"));
}

#[test]
fn loongarch_mirrors_the_live_relocation_in_ks3() {
    let backend = architecture_backend("loongarch64", "arm");
    assert!(backend.contains("csrwr {shadow}, 0x33"));
    assert!(backend.contains("move $r21, {relocation}"));
    assert!(backend.contains("csrrd {shadow}, 0x33"));
}

fn architecture_backend<'source>(start: &str, end: &str) -> &'source str {
    REGISTER
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
