use std::{fs, path::Path};

#[test]
fn manifests_expose_only_the_supported_dynamic_features() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_dir = workspace_dir(percpu_dir);

    assert_eq!(
        feature_names(&read(&percpu_dir.join("Cargo.toml"))),
        ["host-test"],
        "ax-percpu must expose only its host-side dynamic-area fixture"
    );
    assert_eq!(
        feature_names(&read(
            &workspace_dir.join("components/cpu-local/Cargo.toml")
        )),
        ["host-test", "tls"],
        "cpu-local must expose only register-mode and host-test capabilities"
    );
    assert!(
        !read(&workspace_dir.join("components/percpu/percpu_macros/Cargo.toml"))
            .contains("[features]"),
        "ax-percpu-macros must not carry storage-mode features"
    );
}

#[test]
fn runtime_cpu_areas_have_one_template_and_no_legacy_linker_abi() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_dir = workspace_dir(percpu_dir);

    for removed in [
        "components/percpu/percpu/src/linked_layout.rs",
        "components/percpu/percpu/src/custom/mod.rs",
        "components/percpu/percpu/src/naive.rs",
        "components/percpu/percpu/test_percpu.x",
        "components/percpu/percpu/test_percpu_custom.x",
        "components/percpu/percpu_macros/src/naive.rs",
        "platforms/someboot/src/smp/legacy.rs",
        "platforms/someboot/src/smp/prealloc.rs",
        "os/arceos/modules/axhal/axplat.lds.S",
        "virtualization/axvm/percpu-test.x",
    ] {
        assert!(
            !workspace_dir.join(removed).exists(),
            "unsupported per-CPU compatibility file remains: {removed}"
        );
    }

    for required in [
        "components/percpu/percpu/host-test.ld",
        "components/scope-local/host-test.ld",
        "os/arceos/modules/axtask/host-test.ld",
        "platforms/someboot/src/smp/layout.rs",
    ] {
        assert!(
            workspace_dir.join(required).is_file(),
            "missing dynamic-only implementation file: {required}"
        );
    }

    for linker in [
        "components/percpu/percpu/host-test.ld",
        "components/scope-local/host-test.ld",
        "os/arceos/modules/axtask/host-test.ld",
        "platforms/someboot/src/ld/data.ld",
        "platforms/axplat-dyn/link.ld",
        "os/arceos/modules/axruntime/runtime.ld",
    ] {
        let path = workspace_dir.join(linker);
        let source = read(&path);
        for forbidden in [
            concat!(".ax_", "percpu"),
            concat!("__A", "X_"),
            concat!("__ax_", "percpu"),
            concat!("_percpu_", "start"),
            concat!("_percpu_", "end"),
            concat!("_percpu_", "load"),
            concat!(".percpu_", "data"),
        ] {
            assert!(
                !source.contains(forbidden),
                "{} retains legacy linker token {forbidden}",
                path.display()
            );
        }
    }

    let template = read(&workspace_dir.join("platforms/someboot/src/ld/data.ld"));
    for section in [".percpu.template", ".percpu.init", ".percpu.align"] {
        assert!(
            template.contains(section),
            "someboot must retain the canonical {section} section"
        );
    }
    assert!(
        !template.contains("CPU_NUM"),
        "the linker must not replicate runtime CPU areas"
    );
}

#[test]
fn layout_initialization_uses_only_the_neutral_c_abi() {
    let percpu_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_dir = workspace_dir(percpu_dir);
    let provider = read(&percpu_dir.join("src/ffi.rs"));
    let consumer = read(&workspace_dir.join("platforms/someboot/src/smp/mod.rs"));

    let symbol = "__percpu_initialize_layout";
    assert!(provider.contains(symbol), "missing C ABI provider {symbol}");
    assert!(consumer.contains(symbol), "missing C ABI consumer {symbol}");

    for legacy_symbol in [
        "__percpu_initialize_layout_v2",
        "__percpu_image_register_mode_v1",
        concat!("__ax_", "percpu_initialize_layout_v2"),
        concat!("__ax_", "percpu_image_register_mode_v1"),
    ] {
        assert!(
            !provider.contains(legacy_symbol) && !consumer.contains(legacy_symbol),
            "legacy C ABI symbol remains: {legacy_symbol}"
        );
    }
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

fn feature_names(manifest: &str) -> Vec<&str> {
    let features = manifest
        .split_once("[features]")
        .expect("manifest must contain a features table")
        .1
        .split_once("\n[")
        .map_or_else(
            || manifest.split_once("[features]").unwrap().1,
            |(table, _)| table,
        );

    let mut names = features
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.trim()))
        .collect::<Vec<_>>();
    names.sort_unstable();
    names
}
