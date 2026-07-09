#[test]
fn loongarch_vcpu_manifest_has_no_ax_runtime_dependencies() {
    let manifest = include_str!("../Cargo.toml");

    for forbidden in [
        "ax-errno",
        "ax-memory-addr",
        "ax-crate-interface",
        "axvm-types",
        "ax-percpu",
        "ax-kspin",
        "axdevice_base",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "loongarch_vcpu core must not depend on {forbidden}"
        );
    }
}

#[test]
fn loongarch_vcpu_consumers_use_target_specific_dependencies() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("loongarch_vcpu should live two levels under workspace root");
    let mut manifests = Vec::new();
    collect_manifests(workspace, &mut manifests);

    for manifest_path in manifests {
        if manifest_path == workspace.join("Cargo.toml")
            || manifest_path == std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")
        {
            continue;
        }

        let manifest = std::fs::read_to_string(&manifest_path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", manifest_path.display()));
        let mut section = "";
        for line in manifest.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                section = trimmed;
                if loongarch_vcpu_dependency_section(section) {
                    assert!(
                        target_specific_section(section),
                        "{} depends on loongarch_vcpu from non-target-specific section {section}",
                        manifest_path.display()
                    );
                }
                continue;
            }
            if trimmed.starts_with("loongarch_vcpu") {
                assert!(
                    target_specific_section(section),
                    "{} depends on loongarch_vcpu from non-target-specific section {section}",
                    manifest_path.display()
                );
            }
        }
    }
}

fn loongarch_vcpu_dependency_section(section: &str) -> bool {
    section == "[dependencies.loongarch_vcpu]"
        || section.starts_with("[dependencies.loongarch_vcpu.")
        || section.contains(".dependencies.loongarch_vcpu")
}

fn target_specific_section(section: &str) -> bool {
    section.starts_with("[target.")
}

fn collect_manifests(dir: &std::path::Path, manifests: &mut Vec<std::path::PathBuf>) {
    let skip_dirs = [".git", "target"];
    for entry in std::fs::read_dir(dir).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", dir.display());
    }) {
        let entry = entry.expect("read directory entry");
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if path.is_dir() {
            if !skip_dirs.contains(&name) {
                collect_manifests(&path, manifests);
            }
        } else if name == "Cargo.toml" {
            manifests.push(path);
        }
    }
}
