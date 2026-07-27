use std::{fs, path::Path};

const LINUX_CURRENT_IMAGES: &[&str] = &[
    "os/StarryOS/kernel/Cargo.toml",
    "os/StarryOS/starryos/Cargo.toml",
    "os/StarryOS/lkm/hello/Cargo.toml",
    "os/StarryOS/lkm/kprobe_test/Cargo.toml",
];

const UNIKERNEL_TLS_IMAGES: &[&str] = &["os/axvisor/Cargo.toml"];
const REGISTER_MODE_NEUTRAL_LIBRARIES: &[&str] = &["virtualization/axvm/Cargo.toml"];

const ARCEOS_UNIKERNEL_DEFAULT_CONSUMERS: &[&str] = &[
    "apps/arceos/arce_agent/Cargo.toml",
    "apps/arceos/helloworld/Cargo.toml",
    "apps/arceos/httpclient/Cargo.toml",
    "apps/arceos/httpserver/Cargo.toml",
    "apps/arceos/io_test/Cargo.toml",
    "apps/arceos/shell/Cargo.toml",
    "apps/arceos/thread_test/Cargo.toml",
    "apps/arceos/tokio_test/Cargo.toml",
    "test-suit/arceos/axtest/sg2002-usb-msc/Cargo.toml",
    "test-suit/arceos/axtest/smoke/Cargo.toml",
    "test-suit/arceos/rust/Cargo.toml",
];

#[test]
fn workspace_dependency_does_not_impose_an_image_register_mode() {
    let workspace = workspace_root();
    let manifest = read_manifest(&workspace, "Cargo.toml");
    let dependency = inline_dependencies(&manifest, "ax-std")
        .into_iter()
        .next()
        .expect("workspace must declare ax-std");

    assert!(dependency.contains("default-features = false"));
}

#[test]
fn arceos_unikernels_explicitly_retain_the_compatibility_default() {
    let workspace = workspace_root();

    for relative_path in ARCEOS_UNIKERNEL_DEFAULT_CONSUMERS {
        let manifest = read_manifest(&workspace, relative_path);
        let dependencies = inline_dependencies(&manifest, "ax-std");
        assert!(
            !dependencies.is_empty(),
            "{relative_path} must declare ax-std"
        );
        for dependency in dependencies {
            assert!(
                dependency.contains("\"default\""),
                "{relative_path} must explicitly select ax-std/default"
            );
        }
    }
}

#[test]
fn linux_current_images_do_not_select_unikernel_tls_defaults() {
    let workspace = workspace_root();

    for relative_path in LINUX_CURRENT_IMAGES {
        let manifest = read_manifest(&workspace, relative_path);
        let dependencies = inline_dependencies(&manifest, "ax-std");
        assert!(
            !dependencies.is_empty(),
            "{relative_path} must declare ax-std"
        );

        for dependency in dependencies {
            assert!(
                dependency.contains("default-features = false")
                    && !dependency.contains("\"default\"")
                    && !dependency.contains("\"tls\""),
                "{relative_path} must leave ax-std/default and tls disabled"
            );
        }
    }
}

#[test]
fn axvisor_explicitly_selects_unikernel_tls_without_defaults() {
    let workspace = workspace_root();

    for relative_path in UNIKERNEL_TLS_IMAGES {
        let manifest = read_manifest(&workspace, relative_path);
        let dependencies = inline_dependencies(&manifest, "ax-std");
        assert!(
            !dependencies.is_empty(),
            "{relative_path} must declare ax-std"
        );

        for dependency in dependencies {
            assert!(
                dependency.contains("default-features = false")
                    && !dependency.contains("\"default\"")
                    && dependency.contains("\"tls\""),
                "{relative_path} must explicitly select ax-std/tls"
            );
        }
    }
}

#[test]
fn reusable_virtualization_libraries_do_not_select_the_host_register_mode() {
    let workspace = workspace_root();

    for relative_path in REGISTER_MODE_NEUTRAL_LIBRARIES {
        let manifest = read_manifest(&workspace, relative_path);
        for dependency in inline_dependencies(&manifest, "ax-std") {
            assert!(
                dependency.contains("default-features = false")
                    && !dependency.contains("\"default\"")
                    && !dependency.contains("\"tls\""),
                "{relative_path} must leave TLS selection to the final image"
            );
        }
        for dependency in inline_dependencies(&manifest, "ax-percpu") {
            assert!(!dependency.contains("arm-el2"));
        }
    }
}

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("cpu-local must be nested under the workspace components directory")
        .to_path_buf()
}

fn read_manifest(workspace: &Path, relative_path: &str) -> String {
    fs::read_to_string(workspace.join(relative_path))
        .unwrap_or_else(|error| panic!("failed to read {relative_path}: {error}"))
}

fn inline_dependencies<'manifest>(
    manifest: &'manifest str,
    dependency: &str,
) -> Vec<&'manifest str> {
    let declaration = format!("{dependency} = {{");
    let mut remaining = manifest;
    let mut dependencies = Vec::new();

    while let Some(start) = remaining.find(&declaration) {
        let dependency = &remaining[start..];
        let end = dependency
            .find('}')
            .expect("inline Cargo dependency must have a closing brace");
        dependencies.push(&dependency[..=end]);
        remaining = &dependency[end + 1..];
    }

    dependencies
}
