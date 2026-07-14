use std::{fs, path::Path};

#[cfg(all(
    feature = "custom-base",
    not(feature = "host-test"),
    not(feature = "linked-template"),
    not(feature = "sp-naive"),
    not(target_os = "none")
))]
#[ax_percpu::def_percpu]
static HOST_WITHOUT_STORAGE_FIXTURE: usize = 1;

#[cfg(all(
    feature = "custom-base",
    not(feature = "host-test"),
    not(feature = "linked-template"),
    not(feature = "sp-naive"),
    not(target_os = "none")
))]
#[test]
#[should_panic(expected = "explicit host-test storage fixture")]
fn unconfigured_host_consumer_links_but_cannot_invent_cpu_local_storage() {
    let _ = HOST_WITHOUT_STORAGE_FIXTURE.offset();
}

#[test]
fn architecture_register_code_is_owned_by_ax_cpu_local() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_dir = percpu_dir
        .ancestors()
        .nth(3)
        .expect("ax-percpu must remain under components/percpu/percpu");
    let cpu_local_dir = workspace_dir.join("components/ax-cpu-local");

    assert!(
        cpu_local_dir.join("Cargo.toml").is_file(),
        "components/ax-cpu-local must own the architecture register boundary"
    );
    assert!(
        !percpu_dir.join("src/imp.rs").exists(),
        "ax-percpu must name linked storage explicitly instead of retaining an architecture imp \
         module"
    );
    assert!(
        percpu_dir.join("src/linked_layout.rs").is_file(),
        "the linker-owned CPU-area backend must remain explicit"
    );

    let manifest = read_file(&percpu_dir.join("Cargo.toml"));
    assert!(
        manifest.contains("ax-cpu-local"),
        "ax-percpu must depend on ax-cpu-local"
    );
    for forbidden_dependency in ["ax-task", "ax-runtime", "ax-hal"] {
        assert!(
            !manifest.contains(forbidden_dependency),
            "ax-percpu must not depend on {forbidden_dependency}"
        );
    }

    let cpu_local_manifest = read_file(&cpu_local_dir.join("Cargo.toml"));
    assert!(
        !cpu_local_manifest.contains("[dependencies]") && !cpu_local_manifest.contains("[target.'"),
        "ax-cpu-local must remain a zero-dependency architecture leaf"
    );

    let forbidden = [
        "asm!",
        "global_asm!",
        "ia32_gs_base",
        "tpidr_el",
        "sscratch",
        "$r21",
        "gs:[",
        "mrc p15",
        "mcr p15",
        "_percpu_base_ptr",
    ];
    for source_dir in [
        percpu_dir.join("src"),
        percpu_dir
            .parent()
            .expect("ax-percpu must have a parent directory")
            .join("percpu_macros/src"),
    ] {
        for source in rust_sources(&source_dir) {
            let contents = read_file(&source).to_ascii_lowercase();
            for token in forbidden {
                assert!(
                    !contents.contains(token),
                    "{} must not contain architecture register token {token:?}",
                    source.display()
                );
            }
            let register_words = contents
                .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                .filter(|word| !word.is_empty());
            for word in register_words {
                assert!(
                    !matches!(word, "gp" | "r21" | "sscratch") && !word.starts_with("tpidr_"),
                    "{} must not name architecture register {word:?}",
                    source.display()
                );
            }
        }
    }

    let legacy_api = [
        "pub fn init_percpu_reg",
        "pub fn read_percpu_reg",
        "pub unsafe fn write_percpu_reg",
        "pub fn percpu_area_base",
    ];
    for source in rust_sources(&percpu_dir.join("src")) {
        let contents = read_file(&source);
        for signature in legacy_api {
            assert!(
                !contents.contains(signature),
                "{} must not reintroduce legacy CPU-register API {signature:?}",
                source.display()
            );
        }
    }

    let macro_arch = read_file(
        &percpu_dir
            .parent()
            .expect("ax-percpu must have a parent directory")
            .join("percpu_macros/src/arch.rs"),
    );
    for leaked_configuration in ["target_arch", "ax-cpu-local/", "ax_cpu_local/"] {
        assert!(
            !macro_arch.contains(leaked_configuration),
            "generated per-CPU access must not leak leaf configuration {leaked_configuration:?}"
        );
    }

    let naive_macro = read_file(
        &percpu_dir
            .parent()
            .expect("ax-percpu must have a parent directory")
            .join("percpu_macros/src/naive.rs"),
    );
    assert!(
        !naive_macro.contains("current_symbol_ptr"),
        "single-CPU access must not read an uninitialized architecture CPU-local anchor"
    );
}

#[test]
fn custom_storage_selects_linked_template_metadata_explicitly() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_dir = percpu_dir
        .ancestors()
        .nth(3)
        .expect("ax-percpu must remain under components/percpu/percpu");
    let manifest = read_file(&percpu_dir.join("Cargo.toml"));
    let custom_storage = read_file(&percpu_dir.join("src/custom/mod.rs"));
    let cpu_local_header = read_file(&workspace_dir.join("components/ax-cpu-local/src/header.rs"));
    let cpu_local_symbol = read_file(&workspace_dir.join("components/ax-cpu-local/src/symbol.rs"));
    let platform_manifest = read_file(&workspace_dir.join("platforms/axplat-dyn/Cargo.toml"));

    assert!(
        manifest.contains("linked-template = []"),
        "kernel-linked template metadata must be an explicit capability"
    );
    assert!(
        platform_manifest.contains("features = [\"custom-base\", \"linked-template\"]"),
        "the dynamic platform must explicitly select linked template metadata"
    );
    assert!(
        !custom_storage.contains("target_os") && !custom_storage.contains("target_env"),
        "custom storage must not guess kernel execution from a Rust target triple"
    );
    assert!(
        custom_storage.contains("feature = \"linked-template\"")
            && custom_storage.contains("cpu_area_template_size"),
        "linked custom storage must obtain template metadata from ax-cpu-local"
    );
    assert!(
        cpu_local_header.contains(".percpu_end")
            && cpu_local_header.contains("__AX_CPU_AREA_TEMPLATE_END"),
        "the architecture leaf must own a retained final template sentinel"
    );
    assert!(
        cpu_local_symbol.contains("checked_sub")
            && cpu_local_symbol.contains("cpu_area_template_size"),
        "template size must reject a linker that places the end sentinel before the header"
    );

    for script in [
        workspace_dir.join("platforms/someboot/src/ld/data.ld"),
        workspace_dir.join("os/arceos/modules/axhal/axplat.lds.S"),
        percpu_dir.join("test_percpu.x"),
        percpu_dir.join("test_percpu_custom.x"),
        workspace_dir.join("components/scope-local/percpu.x"),
        workspace_dir.join("virtualization/axvm/percpu-test.x"),
    ] {
        let linker = read_file(&script);
        for contract in [
            "KEEP(*(.ax_percpu.align))",
            "__AX_PERCPU_ALIGNMENT_START",
            "__AX_PERCPU_ALIGNMENT_END",
            "__AX_PERCPU_LINKER_ALIGNMENT_START",
            "__AX_PERCPU_LINKER_ALIGNMENT_END",
            "MAX(64, ALIGNOF(.percpu))",
        ] {
            assert!(
                linker.contains(contract),
                "{} must retain and publish dynamic CPU-area alignment metadata: {contract}",
                script.display()
            );
        }
        let end_sentinel = linker.find("KEEP(*(.percpu_end))").unwrap_or_else(|| {
            panic!("{} must retain the template end sentinel", script.display())
        });
        let legacy_data = linker[..end_sentinel]
            .rfind("*(.percpu")
            .unwrap_or_else(|| panic!("{} must collect ordinary per-CPU data", script.display()));
        assert!(
            legacy_data < end_sentinel,
            "{} must place the end sentinel after every ordinary per-CPU input section",
            script.display()
        );
        if linker.contains("CPU_NUM") || linker.contains("%CPU_NUM%") {
            assert!(
                linker.contains("_percpu_stride")
                    && linker.contains("__AX_CPU_AREA_REQUIRED_ALIGNMENT"),
                "{} must align every reserved area stride to the linked template requirement",
                script.display()
            );
        }
    }
}

#[test]
fn cpu_pin_does_not_create_safe_mutable_aliases() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let area_api = read_file(&percpu_dir.join("src/area.rs"));
    let value_api = read_file(&percpu_dir.join("src/value.rs"));
    let macro_api = read_file(
        &percpu_dir
            .parent()
            .expect("ax-percpu must have a parent directory")
            .join("percpu_macros/src/lib.rs"),
    );
    assert!(
        macro_api.contains(".ax_percpu.align") && macro_api.contains("align_of::<#storage_type>()"),
        "the proc macro must publish each storage object's actual alignment as linker metadata"
    );
    assert!(
        !macro_api.contains("per-CPU symbol alignment exceeds the fixed CPU-area ABI"),
        "the proc macro must not impose the header's 64-byte alignment as a symbol limit"
    );

    for source in [(&value_api, "value API"), (&macro_api, "generated API")] {
        assert!(
            !source.0.contains("pub fn with_current<")
                && !source.0.contains("pub fn with_current("),
            "{} must not expose the historical safe mutable closure API",
            source.1
        );
    }
    assert!(
        value_api.contains("pub unsafe fn with_current_mut_raw"),
        "mutable CPU-local borrows must require an explicit unsafe exclusivity contract"
    );
    assert!(
        value_api.contains("T: Sync") && value_api.contains("pub fn with_current_ref"),
        "safe shared object access must remain restricted to Sync values"
    );

    for atomic in [
        "AtomicBool",
        "AtomicU8",
        "AtomicU16",
        "AtomicU32",
        "AtomicU64",
        "AtomicUsize",
    ] {
        assert!(
            macro_api.contains(atomic),
            "primitive CPU-local templates must use aligned {atomic} storage"
        );
    }
    assert!(
        value_api.contains("T::load(self.current_ptr(pin))")
            && value_api.contains("T::store(self.current_ptr(pin) as *mut T, value)"),
        "safe primitive access must remain atomic because CpuPin does not mask hard IRQs"
    );
    assert!(
        !area_api.contains("pub const fn pin(&self) -> &CpuPin"),
        "a CPU-lifetime area binding must not lend a permanent migration pin after boot"
    );
}

#[test]
fn safe_current_access_requires_a_verified_bound_cpu_pin() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let area_api = read_file(&percpu_dir.join("src/area.rs"));
    let value_api = read_file(&percpu_dir.join("src/value.rs"));
    let library = read_file(&percpu_dir.join("src/lib.rs"));
    let kspin_context = read_file(
        &percpu_dir
            .ancestors()
            .nth(3)
            .expect("ax-percpu must remain under components/percpu/percpu")
            .join("components/kspin/src/context.rs"),
    );

    assert!(
        area_api.contains("pub struct BoundCpuPin") && area_api.contains("pub fn bound_current("),
        "ax-percpu must distinguish migration pinning from a verified live CPU-area binding"
    );
    assert!(
        library.contains("pin: &crate::BoundCpuPin") && value_api.contains("pin: &BoundCpuPin"),
        "every safe current pointer/reference/value accessor must require BoundCpuPin"
    );
    assert!(
        !kspin_context.contains("BoundCpuPin") && kspin_context.contains("CpuPin::new_unchecked()"),
        "ax-kspin may create only the migration proof; it must not forge a bound-area proof"
    );

    let raw_anchor = area_api
        .find("ax_cpu_local::current_area_base_raw(pin)")
        .expect("bound_current must first read the raw architecture value");
    let range_check = area_api[raw_anchor..]
        .find("area_from_runtime_base(runtime_base)")
        .map(|offset| raw_anchor + offset)
        .expect("bound_current must match the raw value against layout range and stride");
    let header_check = area_api[range_check..]
        .find("verify_current(current_area, pin)")
        .map(|offset| range_check + offset)
        .expect("bound_current must validate the immutable header");
    assert!(
        raw_anchor < range_check && range_check < header_check,
        "an unbound register value must be rejected before any header dereference"
    );
}

#[test]
fn cpu_binding_preflights_every_recoverable_error_before_commit() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let area_api = read_file(&percpu_dir.join("src/area.rs"));

    assert!(
        area_api.contains("struct PreparedCurrentBinding"),
        "CPU binding needs an opaque prepared state before publishing the header or register"
    );

    let binding = function_body(&area_api, "pub unsafe fn bind_current(");
    let prepare = binding
        .find("prepare_current_binding(area)?")
        .expect("bind_current must finish recoverable validation before commit");
    let commit = binding
        .find("commit_current_binding(prepared)")
        .expect("bind_current must have one explicit commit point");
    assert!(
        prepare < commit,
        "CPU binding must prepare before its irreversible commit"
    );

    let commit_signature = function_signature(&area_api, "unsafe fn commit_current_binding(");
    assert!(
        !commit_signature.contains("Result"),
        "the irreversible commit API must not advertise recoverable failure"
    );
    let commit_body = function_body(&area_api, "unsafe fn commit_current_binding(");
    assert!(
        commit_body.contains("ax_cpu_local::install_current")
            && commit_body.contains("fatal_current_binding_invariant"),
        "post-publication verification failure must be an explicit fatal invariant"
    );
    assert!(
        !commit_body.contains("return Err") && !commit_body.contains('?'),
        "the irreversible commit path must not expose a recoverable error"
    );
}

#[test]
fn dynamic_hypervisor_feature_selects_the_el2_cpu_anchor_backend() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_dir = percpu_dir
        .ancestors()
        .nth(3)
        .expect("ax-percpu must remain under components/percpu/percpu");
    let platform_manifest = read_file(&workspace_dir.join("platforms/axplat-dyn/Cargo.toml"));

    assert!(
        platform_manifest
            .contains("hv = [\"somehal/hv\", \"ax-cpu/arm-el2\", \"ax-percpu/arm-el2\"]"),
        "axplat-dyn's standalone hv feature must select TPIDR_EL2 in ax-cpu-local through \
         ax-percpu"
    );
}

#[test]
fn linker_contract_places_the_fixed_prefix_at_template_offset_zero() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    for script_name in ["test_percpu.x", "test_percpu_custom.x"] {
        let script = read_file(&percpu_dir.join(script_name));
        let fixed_prefix = script
            .find("KEEP(*(.percpu.000.header))")
            .unwrap_or_else(|| panic!("{script_name} must retain the fixed CPU-area prefix"));
        let ordinary_symbols = script
            .find("SORT_BY_NAME(.percpu.*)")
            .unwrap_or_else(|| panic!("{script_name} must sort ordinary per-CPU symbols"));

        assert!(
            fixed_prefix < ordinary_symbols,
            "{script_name} must place the fixed prefix before ordinary symbols"
        );
        assert!(
            script.contains("__AX_CPU_AREA_PREFIX == _percpu_load_start"),
            "{script_name} must reject any nonzero fixed-prefix template offset"
        );
    }
}

#[test]
fn current_address_is_area_base_plus_template_offset() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let library = read_file(&percpu_dir.join("src/lib.rs"));
    let macro_backend = read_file(
        &percpu_dir
            .parent()
            .expect("ax-percpu must have a parent directory")
            .join("percpu_macros/src/arch.rs"),
    );

    assert!(
        library.contains("pin.area_base().wrapping_add(offset)"),
        "pinned current access must calculate current area base + template offset"
    );
    assert!(
        library.contains("current_area_base_unchecked()")
            && library.contains(".wrapping_add(offset)"),
        "unchecked current access must use the leaf's NonNull area base plus template offset"
    );
    assert!(
        !library.contains("read_current_relocation(pin)")
            && !library.contains("relocation().relocate(symbol_vma)"),
        "ax-percpu current access must not retain relocation + symbol-VMA addressing"
    );
    assert!(
        macro_backend.contains("current_symbol_ptr::<#ty>(#pin, #offset)")
            && macro_backend.contains("current_symbol_ptr_unchecked::<#ty>(#offset)"),
        "the proc macro must pass a template offset, not a link-time VMA, to current access"
    );
}

fn rust_sources(directory: &Path) -> Vec<std::path::PathBuf> {
    let mut sources = Vec::new();
    for entry in fs::read_dir(directory).expect("source directory must be readable") {
        let path = entry.expect("source entry must be readable").path();
        if path.is_dir() {
            sources.extend(rust_sources(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push(path);
        }
    }
    sources.sort();
    sources
}

fn read_file(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

fn function_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let function_start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function signature {signature:?}"));
    let body_start = source[function_start..]
        .find('{')
        .map(|offset| function_start + offset)
        .expect("function must have a body");
    let mut depth = 0usize;
    for (offset, byte) in source[body_start..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[body_start..=body_start + offset];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function body for {signature:?}")
}

fn function_signature<'source>(source: &'source str, signature: &str) -> &'source str {
    let function_start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function signature {signature:?}"));
    let body_start = source[function_start..]
        .find('{')
        .map(|offset| function_start + offset)
        .expect("function must have a body");
    &source[function_start..body_start]
}
