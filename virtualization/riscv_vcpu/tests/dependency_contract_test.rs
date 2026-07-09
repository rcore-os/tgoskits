use std::{fs, path::Path};

const FORBIDDEN_DEPENDENCIES: &[&str] = &[
    "ax-errno",
    "ax-memory-addr",
    "ax-crate-interface",
    "axvm-types",
    "ax-percpu",
    "ax-kspin",
    "axdevice_base",
];

#[test]
fn riscv_vcpu_manifest_has_no_os_dependencies() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(manifest_dir.join("Cargo.toml")).unwrap();

    for forbidden in FORBIDDEN_DEPENDENCIES {
        assert!(
            !manifest.contains(forbidden),
            "riscv_vcpu must not depend on OS/AxVM crate `{forbidden}`"
        );
    }
}

#[test]
fn workspace_consumers_use_target_specific_riscv_vcpu_dependency() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("riscv_vcpu lives under virtualization/");

    let mut bad_manifests = Vec::new();
    visit_manifests(workspace_root, &mut |path| {
        let content = fs::read_to_string(path).unwrap();
        if path == workspace_root.join("Cargo.toml") || path == manifest_dir.join("Cargo.toml") {
            return;
        }
        if has_plain_riscv_vcpu_dependency(&content) {
            bad_manifests.push(
                path.strip_prefix(workspace_root)
                    .unwrap()
                    .display()
                    .to_string(),
            );
        }
    });

    assert!(
        bad_manifests.is_empty(),
        "riscv_vcpu consumers must use target-specific dependencies, found plain dependency in: \
         {bad_manifests:?}"
    );
}

fn visit_manifests(root: &Path, f: &mut impl FnMut(&Path)) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if path.is_dir() {
                if matches!(
                    name.to_str(),
                    Some(".git" | "target" | ".worktrees" | "worktrees")
                ) {
                    continue;
                }
                stack.push(path);
            } else if name == "Cargo.toml" {
                f(&path);
            }
        }
    }
}

fn has_plain_riscv_vcpu_dependency(content: &str) -> bool {
    let mut in_plain_dependencies = false;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') {
            in_plain_dependencies = line == "[dependencies]";
            continue;
        }

        if in_plain_dependencies && line.starts_with("riscv_vcpu") {
            return true;
        }
    }

    false
}
