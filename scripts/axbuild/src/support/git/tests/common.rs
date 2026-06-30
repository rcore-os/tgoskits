use std::{collections::HashMap, path::Path, process::Command};

use cargo_metadata::{Metadata, Package};

pub(super) fn package(root: &Path, name: &str, deps: &[&str]) -> serde_json::Value {
    let root = root.display().to_string();
    serde_json::json!({
        "name": name,
        "version": "0.1.0",
        "id": format!("{name} 0.1.0 (path+file://{root}/crates/{name})"),
        "license": null,
        "license_file": null,
        "description": null,
        "source": null,
        "dependencies": deps
            .iter()
            .map(|dep| {
                serde_json::json!({
                    "name": dep,
                    "source": null,
                    "req": "*",
                    "kind": null,
                    "rename": null,
                    "optional": false,
                    "uses_default_features": true,
                    "features": [],
                    "target": null,
                    "registry": null,
                    "path": format!("{root}/crates/{dep}")
                })
            })
            .collect::<Vec<_>>(),
        "targets": [{
            "kind": ["lib"],
            "crate_types": ["lib"],
            "name": name,
            "src_path": format!("{root}/crates/{name}/src/lib.rs"),
            "edition": "2021",
            "doc": true,
            "doctest": true,
            "test": true
        }],
        "features": HashMap::<String, Vec<String>>::new(),
        "manifest_path": format!("{root}/crates/{name}/Cargo.toml"),
        "metadata": null,
        "publish": null,
        "authors": [],
        "categories": [],
        "keywords": [],
        "readme": null,
        "repository": null,
        "homepage": null,
        "documentation": null,
        "edition": "2021",
        "links": null,
        "default_run": null,
        "rust_version": null
    })
}

pub(super) fn test_workspace() -> (tempfile::TempDir, Metadata, Vec<Package>) {
    let root = tempfile::tempdir().unwrap();
    for package in ["alpha", "beta", "gamma"] {
        std::fs::create_dir_all(root.path().join("crates").join(package).join("src")).unwrap();
    }
    let root_url = root.path().display().to_string();
    let alpha = format!("alpha 0.1.0 (path+file://{root_url}/crates/alpha)");
    let beta = format!("beta 0.1.0 (path+file://{root_url}/crates/beta)");
    let gamma = format!("gamma 0.1.0 (path+file://{root_url}/crates/gamma)");
    let value = serde_json::json!({
        "packages": [
            package(root.path(), "alpha", &[]),
            package(root.path(), "beta", &["alpha"]),
            package(root.path(), "gamma", &["beta"]),
        ],
        "workspace_members": [alpha, beta, gamma],
        "workspace_default_members": [alpha, beta, gamma],
        "resolve": {
            "nodes": [
                {
                    "id": alpha,
                    "dependencies": [],
                    "deps": [],
                    "features": []
                },
                {
                    "id": beta,
                    "dependencies": [alpha],
                    "deps": [{
                        "name": "alpha",
                        "pkg": alpha,
                        "dep_kinds": [{"kind": null, "target": null}]
                    }],
                    "features": []
                },
                {
                    "id": gamma,
                    "dependencies": [beta],
                    "deps": [{
                        "name": "beta",
                        "pkg": beta,
                        "dep_kinds": [{"kind": null, "target": null}]
                    }],
                    "features": []
                }
            ],
            "root": null
        },
        "target_directory": root.path().join("target"),
        "version": 1,
        "workspace_root": root.path(),
        "metadata": null,
    });
    let metadata: Metadata = serde_json::from_value(value).unwrap();
    let workspace_packages = metadata.packages.clone();
    (root, metadata, workspace_packages)
}

pub(super) fn run_git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) fn git_stdout(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}
