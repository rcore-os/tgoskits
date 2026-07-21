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
fn architecture_register_code_is_owned_by_cpu_local() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_dir = percpu_dir
        .ancestors()
        .nth(3)
        .expect("ax-percpu must remain under components/percpu/percpu");
    let cpu_local_dir = workspace_dir.join("components/cpu-local");

    assert!(
        cpu_local_dir.join("Cargo.toml").is_file(),
        "components/cpu-local must own the architecture register boundary"
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
        manifest.contains("cpu-local"),
        "ax-percpu must depend on cpu-local"
    );
    for forbidden_dependency in ["ax-task", "ax-runtime", "ax-hal"] {
        assert!(
            !manifest.contains(forbidden_dependency),
            "ax-percpu must not depend on {forbidden_dependency}"
        );
    }

    let cpu_local_manifest = read_file(&cpu_local_dir.join("Cargo.toml"));
    assert_eq!(
        manifest_dependency_keys(&cpu_local_manifest),
        vec!["trait-ffi"],
        "cpu-local may depend only on the value-only trait-ffi boundary"
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

    assert!(
        !percpu_dir
            .parent()
            .expect("ax-percpu must have a parent directory")
            .join("percpu_macros/src/arch.rs")
            .exists(),
        "percpu_macros must not retain an architecture code-generation module"
    );
    let macro_address = read_file(
        &percpu_dir
            .parent()
            .expect("ax-percpu must have a parent directory")
            .join("percpu_macros/src/address.rs"),
    );
    for leaked_configuration in ["target_arch", "cpu-local/", "cpu_local/"] {
        assert!(
            !macro_address.contains(leaked_configuration),
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
    let cpu_local_header = read_rust_module(&workspace_dir.join("components/cpu-local/src/header"));
    let cpu_local_symbol = read_file(&workspace_dir.join("components/cpu-local/src/symbol.rs"));
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
        "linked custom storage must obtain template metadata from cpu-local"
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
    let area_api = read_rust_module(&percpu_dir.join("src/area"));
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
    let area_api = read_rust_module(&percpu_dir.join("src/area"));
    let value_api = read_file(&percpu_dir.join("src/value.rs"));
    let library = read_file(&percpu_dir.join("src/lib.rs"));
    let scope_local_item = read_file(
        &percpu_dir
            .ancestors()
            .nth(3)
            .expect("ax-percpu must remain under components/percpu/percpu")
            .join("components/scope-local/src/item.rs"),
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
        !scope_local_item.contains("BoundCpuPin")
            && scope_local_item.contains("NoPreempt::new()")
            && scope_local_item.contains("CpuPin::new_unchecked()"),
        "consumers may create only a guarded migration proof; they must not forge a bound-area \
         proof"
    );

    let bound_current = function_body(&area_api, "pub fn bound_current(");
    let capability_query = bound_current
        .find("current_platform_binding()?")
        .expect("bound_current must consume the trait-ffi platform binding capability");
    let layout_check = bound_current[capability_query..]
        .find("area_from_binding(binding)")
        .map(|offset| capability_query + offset)
        .expect("bound_current must match the value-only binding against the frozen layout");
    let header_check = bound_current[layout_check..]
        .find("validate_init(current_area.init_facts())")
        .map(|offset| layout_check + offset)
        .expect("bound_current must validate the immutable header");
    assert!(
        capability_query < layout_check && layout_check < header_check,
        "a platform binding must match the frozen layout before any header dereference"
    );
    assert!(
        !bound_current.contains("current_area_base_raw")
            && !bound_current.contains("area_from_runtime_base"),
        "safe current access must not reinterpret raw architecture-register values"
    );
}

#[test]
fn cpu_binding_is_owned_only_by_the_platform_boundary() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let area_api = read_rust_module(&percpu_dir.join("src/area"));

    assert!(
        area_api.contains("pub fn binding(self) -> CpuBindingV1")
            && area_api.contains("pub fn bound_current("),
        "ax-percpu must expose value-only area facts and verify the platform-published binding"
    );
    for forbidden in [
        "pub unsafe fn bind_current(",
        "InstalledPerCpuArea",
        "raw::install_binding",
        "current_area_base_raw",
    ] {
        assert!(
            !area_api.contains(forbidden),
            "ax-percpu must not own platform binding primitive {forbidden:?}"
        );
    }
}

#[test]
fn final_image_mode_is_distinct_from_the_live_aarch64_host_level() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_dir = percpu_dir
        .ancestors()
        .nth(3)
        .expect("ax-percpu must remain under components/percpu/percpu");
    let manifest = read_file(&percpu_dir.join("Cargo.toml"));
    let area_api = read_rust_module(&percpu_dir.join("src/area"));
    let someboot = read_file(&workspace_dir.join("platforms/someboot/src/smp/mod.rs"));
    let aarch64 = read_file(&workspace_dir.join("platforms/someboot/src/arch/aarch64/mod.rs"));

    assert!(
        manifest.contains("tls = [\"cpu-local/tls\"]") && !manifest.contains("arm-el2"),
        "ax-percpu may select final-image TLS semantics, but the CPU-local leaf must discover the \
         live AArch64 host level instead of encoding it as a Cargo feature"
    );
    assert!(
        area_api.contains("pub const fn new(") && !area_api.contains("pub fn for_image("),
        "production initialization must supply an explicit live host level instead of using an \
         ambiguous final-image default"
    );
    assert!(
        someboot.contains("Arch::cpu_local_host_level()")
            && aarch64.contains("CurrentEL.read(CurrentEL::EL)")
            && aarch64.contains("1 => 0")
            && aarch64.contains("2 => 1"),
        "final-high AArch64 initialization must derive HostLevelV1 from the live exception level"
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
        let generated_storage = script
            .find("SORT_BY_NAME(.percpu.storage*)")
            .unwrap_or_else(|| panic!("{script_name} must retain generated storage explicitly"));
        let ordinary_symbols = script
            .find("SORT_BY_NAME(.percpu.*)")
            .unwrap_or_else(|| panic!("{script_name} must sort ordinary per-CPU symbols"));

        assert!(
            fixed_prefix < generated_storage && generated_storage < ordinary_symbols,
            "{script_name} must place the fixed prefix before generated storage and other symbols"
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
            .join("percpu_macros/src/address.rs"),
    );

    assert!(
        library.contains("pin.area_base().wrapping_add(offset)"),
        "pinned current access must calculate current area base + template offset"
    );
    assert!(
        library.contains("platform::current_cpu_binding()")
            && library.contains("binding.area_base")
            && library.contains(".wrapping_add(offset)"),
        "unchecked current access must consume the platform binding before adding the template \
         offset"
    );
    assert!(
        !library.contains("current_area_base_raw")
            && !library.contains("current_area_base_unchecked")
            && !library.contains("PerCpuRelocation"),
        "ax-percpu current access must not retain a raw register or relocation API"
    );
    assert!(
        macro_backend.contains("current_symbol_ptr::<#ty>(#pin, #offset)")
            && macro_backend.contains("current_symbol_ptr_unchecked::<#ty>(#offset)"),
        "the proc macro must pass a template offset, not a link-time VMA, to current access"
    );
    assert!(
        !macro_backend.contains("gen_symbol_vma")
            && !library.contains("symbol_vma")
            && library.contains("checked_sub(crate::percpu_template_base())"),
        "the public and generated APIs must expose only load-relative template offsets"
    );
}

#[test]
fn cpu_values_are_constructed_after_final_relocation_instead_of_copied() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_dir = percpu_dir
        .ancestors()
        .nth(3)
        .expect("ax-percpu must remain under components/percpu/percpu");
    let macro_api = read_file(
        &percpu_dir
            .parent()
            .expect("ax-percpu must have a parent directory")
            .join("percpu_macros/src/lib.rs"),
    );
    let initialization = read_file(&percpu_dir.join("src/initialization.rs"));
    let someboot = read_file(&workspace_dir.join("platforms/someboot/src/smp/mod.rs"));
    let legacy = read_file(&workspace_dir.join("platforms/someboot/src/smp/legacy.rs"));
    let prealloc = read_file(&workspace_dir.join("platforms/someboot/src/smp/prealloc.rs"));
    let prime_entry = read_file(&workspace_dir.join("platforms/someboot/src/lib.rs"));
    let linker = read_file(&workspace_dir.join("platforms/someboot/src/ld/data.ld"));

    assert!(
        macro_api.contains("MaybeUninit<#storage_type>")
            && macro_api.contains("unsafe(link_section = \".percpu.storage\")")
            && !macro_api.contains(".percpu.data")
            && macro_api.contains(".ax_percpu.init")
            && macro_api.contains("PerCpuInitRegistration"),
        "def_percpu must reserve uninitialized storage and register a typed final-address \
         initializer"
    );
    assert!(
        initialization.contains("pub unsafe fn initialize_layout(")
            && initialization.contains("validate_init_records")
            && initialization.contains("validate_prefixes")
            && initialization.contains("initialize_area"),
        "ax-percpu must validate the complete relative init table before constructing any area"
    );
    assert!(
        linker.contains("__AX_PERCPU_INIT_START")
            && linker.contains("KEEP(*(.ax_percpu.init))")
            && linker.contains("__AX_PERCPU_INIT_END"),
        "the final image must retain and bound the typed per-CPU initializer table"
    );

    for (name, source) in [("legacy", &legacy), ("prealloc", &prealloc)] {
        let allocation = function_body(source, "pub fn alloc_percpu(");
        assert!(
            !allocation.contains("copy_nonoverlapping")
                && !allocation.contains("PerCpuMeta {")
                && !allocation.contains("publish_runtime_percpu"),
            "someboot {name} early allocation must reserve raw storage without copying values or \
             publishing metadata"
        );
    }
    assert!(
        someboot.contains("pub(crate) fn initialize_percpu_layout(")
            && someboot.contains("__ax_percpu_initialize_layout_v2")
            && someboot.contains("publish_runtime_percpu"),
        "someboot must finalize per-CPU values and metadata through the value-only ax-percpu ABI"
    );

    let prime = function_body(&prime_entry, "fn prime_entry(");
    let initialize = prime
        .find("smp::initialize_percpu_layout()")
        .expect("prime_entry must initialize the final high-address per-CPU layout");
    let metadata_read = prime
        .find("smp::cpu_meta(cpu_idx)")
        .expect("prime_entry must obtain its final stack from initialized metadata");
    assert!(
        initialize < metadata_read,
        "final per-CPU initialization must precede the first metadata read"
    );
}

#[test]
fn typed_initializer_registration_is_an_explicit_unsafe_trust_boundary() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let initialization = read_file(&percpu_dir.join("src/initialization.rs"));
    let macro_api = read_file(
        &percpu_dir
            .parent()
            .expect("ax-percpu must have a parent directory")
            .join("percpu_macros/src/lib.rs"),
    );

    assert!(
        initialization.contains("pub const unsafe fn new(\n        storage_address:")
            && initialization.contains("pub const unsafe fn new(describe:"),
        "safe callers must not be able to forge mutable or nondeterministic initializer records"
    );
    assert!(
        initialization.contains("same descriptor on every invocation")
            && macro_api.contains("PerCpuInitDescriptor::new(")
            && macro_api.contains("PerCpuInitRegistration::new(#descriptor_name)"),
        "the generated unsafe registration must document and uphold descriptor determinism"
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

fn read_rust_module(path: &Path) -> String {
    let leaf = path.with_extension("rs");
    if leaf.is_file() {
        return read_file(&leaf);
    }

    let mut contents = String::new();
    for source in rust_sources(path) {
        contents.push_str(&read_file(&source));
        contents.push('\n');
    }
    contents
}

fn manifest_dependency_keys(manifest: &str) -> Vec<&str> {
    let mut in_dependencies = false;
    let mut dependencies = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_dependencies = line == "[dependencies]";
            continue;
        }
        if !in_dependencies || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = line.split_once('=') {
            dependencies.push(name.trim().trim_end_matches(".workspace"));
        }
    }
    dependencies.sort_unstable();
    dependencies
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
