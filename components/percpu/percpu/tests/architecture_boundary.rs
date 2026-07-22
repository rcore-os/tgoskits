use std::{fs, path::Path};

#[test]
fn architecture_register_code_is_owned_by_cpu_local() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_dir = workspace_dir(percpu_dir);
    let cpu_local_dir = workspace_dir.join("components/cpu-local");

    assert!(cpu_local_dir.join("src/register/mod.rs").is_file());
    assert!(percpu_dir.join("src/template.rs").is_file());
    for removed in [
        "src/imp.rs",
        "src/linked_layout.rs",
        "src/custom/mod.rs",
        "src/naive.rs",
    ] {
        assert!(
            !percpu_dir.join(removed).exists(),
            "ax-percpu retains unsupported backend {removed}"
        );
    }

    let manifest = read(&percpu_dir.join("Cargo.toml"));
    assert!(manifest.contains("cpu-local"));
    for forbidden_dependency in ["ax-task", "ax-runtime", "ax-hal"] {
        assert!(
            !manifest.contains(forbidden_dependency),
            "ax-percpu must not depend on {forbidden_dependency}"
        );
    }

    let forbidden_registers = [
        "asm!",
        "global_asm!",
        "ia32_gs_base",
        "tpidr_el",
        "sscratch",
        "$r21",
        "gs:[",
        "mrc p15",
        "mcr p15",
    ];
    for source_dir in [
        percpu_dir.join("src"),
        percpu_dir.parent().unwrap().join("percpu_macros/src"),
    ] {
        for source in rust_sources(&source_dir) {
            let contents = read(&source).to_ascii_lowercase();
            for token in forbidden_registers {
                assert!(
                    !contents.contains(token),
                    "{} leaks architecture register token {token:?}",
                    source.display()
                );
            }
        }
    }
}

#[test]
fn linker_contract_uses_one_neutral_template() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_dir = workspace_dir(percpu_dir);
    let macro_api = read(
        &percpu_dir
            .parent()
            .unwrap()
            .join("percpu_macros/src/lib.rs"),
    );
    let header = read(&workspace_dir.join("components/cpu-local/src/header/area.rs"));
    let symbol = read(&workspace_dir.join("components/cpu-local/src/symbol.rs"));

    for section in [".percpu.template.storage", ".percpu.init", ".percpu.align"] {
        assert!(
            macro_api.contains(section),
            "def_percpu must emit canonical section {section}"
        );
    }
    assert!(header.contains(".percpu.template.header"));
    assert!(header.contains(".percpu.template.end"));
    assert!(header.contains("__CPU_LOCAL_AREA_PREFIX"));
    assert!(header.contains("__CPU_LOCAL_TEMPLATE_END"));
    assert!(symbol.contains("checked_sub") && symbol.contains("checked_add"));

    for script in [
        percpu_dir.join("host-test.ld"),
        workspace_dir.join("components/scope-local/host-test.ld"),
        workspace_dir.join("platforms/someboot/src/ld/data.ld"),
    ] {
        let linker = read(&script);
        let prefix = linker
            .find("KEEP(*(.percpu.template.header))")
            .unwrap_or_else(|| panic!("{} must retain the fixed prefix", script.display()));
        let storage = linker
            .find("SORT_BY_NAME(.percpu.template.storage*)")
            .unwrap_or_else(|| panic!("{} must retain generated storage", script.display()));
        let end = linker
            .find("KEEP(*(.percpu.template.end))")
            .unwrap_or_else(|| panic!("{} must retain the end sentinel", script.display()));
        assert!(prefix < storage && storage < end);
        for contract in [
            "__PERCPU_INIT_START",
            "__PERCPU_INIT_END",
            "__PERCPU_ALIGN_START",
            "__PERCPU_ALIGN_END",
            "__PERCPU_TEMPLATE_ALIGN_START",
            "__PERCPU_TEMPLATE_ALIGN_END",
            "MAX(64, ALIGNOF(.percpu.template))",
        ] {
            assert!(
                linker.contains(contract),
                "{} is missing linker contract {contract}",
                script.display()
            );
        }
    }
}

#[test]
fn cpu_pin_does_not_create_safe_mutable_aliases() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let area_api = read_rust_module(&percpu_dir.join("src/area"));
    let value_api = read(&percpu_dir.join("src/value.rs"));
    let macro_api = read(
        &percpu_dir
            .parent()
            .unwrap()
            .join("percpu_macros/src/lib.rs"),
    );

    assert!(macro_api.contains("align_of::<#storage_type>()"));
    for atomic in [
        "AtomicBool",
        "AtomicU8",
        "AtomicU16",
        "AtomicU32",
        "AtomicU64",
        "AtomicUsize",
    ] {
        assert!(macro_api.contains(atomic));
    }
    assert!(value_api.contains("pub unsafe fn with_current_mut_raw"));
    assert!(value_api.contains("T: Sync") && value_api.contains("pub fn with_current_ref"));
    assert!(value_api.contains("T::load(self.current_ptr(pin))"));
    assert!(value_api.contains("T::store(self.current_ptr(pin) as *mut T, value)"));
    assert!(!area_api.contains("pub const fn pin(&self) -> &CpuPin"));
}

#[test]
fn safe_current_access_requires_a_verified_bound_cpu_pin() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let area_api = read_rust_module(&percpu_dir.join("src/area"));
    let value_api = read(&percpu_dir.join("src/value.rs"));
    let library = read(&percpu_dir.join("src/lib.rs"));

    assert!(area_api.contains("pub struct BoundCpuPin"));
    assert!(area_api.contains("pub fn bound_current("));
    assert!(value_api.contains("pin: &BoundCpuPin<'_>"));
    assert!(library.contains("pin.area_base().wrapping_add(offset)"));
    assert!(library.contains("platform::current_cpu_binding()"));
    assert!(library.contains("checked_sub(crate::template_base())"));
    assert!(!library.contains("current_area_base_raw"));
}

#[test]
fn typed_values_are_constructed_only_in_final_runtime_areas() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_dir = workspace_dir(percpu_dir);
    let macro_api = read(
        &percpu_dir
            .parent()
            .unwrap()
            .join("percpu_macros/src/lib.rs"),
    );
    let initialization = read(&percpu_dir.join("src/initialization.rs"));
    let someboot = read(&workspace_dir.join("platforms/someboot/src/smp/mod.rs"));
    let layout = read(&workspace_dir.join("platforms/someboot/src/smp/layout.rs"));

    assert!(macro_api.contains("MaybeUninit<#storage_type>"));
    assert!(macro_api.contains("PerCpuInitRegistration"));
    assert!(initialization.contains("validate_init_records"));
    assert!(initialization.contains("validate_prefixes"));
    assert!(initialization.contains("initialize_area"));
    assert!(someboot.contains("__percpu_initialize_layout_v2"));
    assert!(someboot.contains("publish_runtime_cpu_areas"));

    let allocation = function_body(&layout, "pub fn allocate_cpu_areas(");
    assert!(!allocation.contains("copy_nonoverlapping"));
    assert!(!allocation.contains("PerCpuMeta {"));
    assert!(!allocation.contains("publish_runtime_cpu_areas"));
}

#[test]
fn typed_initializer_registration_is_an_explicit_unsafe_boundary() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let initialization = read(&percpu_dir.join("src/initialization.rs"));
    let macro_api = read(
        &percpu_dir
            .parent()
            .unwrap()
            .join("percpu_macros/src/lib.rs"),
    );

    assert!(initialization.contains("pub const unsafe fn new(\n        storage_address:"));
    assert!(initialization.contains("pub const unsafe fn new(describe:"));
    assert!(initialization.contains("same descriptor on every invocation"));
    assert!(macro_api.contains("PerCpuInitDescriptor::new("));
    assert!(macro_api.contains("PerCpuInitRegistration::new(#descriptor_name)"));
}

fn workspace_dir(crate_dir: &Path) -> &Path {
    crate_dir
        .ancestors()
        .nth(3)
        .expect("ax-percpu must remain under components/percpu/percpu")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

fn read_rust_module(path: &Path) -> String {
    let mut files = rust_sources(path);
    files.sort();
    files
        .into_iter()
        .map(|source| read(&source))
        .collect::<Vec<_>>()
        .join("\n")
}

fn rust_sources(directory: &Path) -> Vec<std::path::PathBuf> {
    let mut pending = vec![directory.to_path_buf()];
    let mut sources = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
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

fn function_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function {signature}"));
    let body = &source[start..];
    let open = body.find('{').expect("function must have a body");
    let mut depth = 0usize;
    for (index, byte) in body.as_bytes()[open..].iter().copied().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &body[open + 1..open + index];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function body for {signature}")
}
