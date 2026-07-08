use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use cargo_metadata::{Metadata, Package};
use serde_json::Value;

use crate::clippy::{
    check::{ClippyCheck, ClippyDepsMode},
    expand::expand_clippy_checks,
    runner::CargoRunner,
    selection::SelectedClippyPackage,
};

pub(super) fn pkg(
    name: &str,
    id: &str,
    features: &[(&str, &[&str])],
    docs_rs_targets: Option<&[&str]>,
) -> Package {
    let metadata = docs_rs_targets.map(|targets| {
        serde_json::json!({
            "docs.rs": {
                "targets": targets,
            }
        })
    });
    let value = serde_json::json!({
        "name": name,
        "version": "0.1.0",
        "id": id,
        "license": null,
        "license_file": null,
        "description": null,
        "source": null,
        "dependencies": [],
        "targets": [{
            "kind": ["lib"],
            "crate_types": ["lib"],
            "name": name,
            "src_path": format!("/tmp/{name}/src/lib.rs"),
            "edition": "2021",
            "doc": true,
            "doctest": true,
            "test": true
        }],
        "features": features.iter().map(|(k, v)| ((*k).to_string(), v.iter().map(|item| (*item).to_string()).collect::<Vec<_>>())).collect::<HashMap<_, _>>(),
        "manifest_path": format!("/tmp/{name}/Cargo.toml"),
        "metadata": metadata,
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
    });

    serde_json::from_value(value).unwrap()
}

pub(super) fn pkg_with_metadata(
    name: &str,
    id: &str,
    features: &[(&str, &[&str])],
    metadata: Value,
) -> Package {
    let value = serde_json::json!({
        "name": name,
        "version": "0.1.0",
        "id": id,
        "license": null,
        "license_file": null,
        "description": null,
        "source": null,
        "dependencies": [],
        "targets": [{
            "kind": ["lib"],
            "crate_types": ["lib"],
            "name": name,
            "src_path": format!("/tmp/{name}/src/lib.rs"),
            "edition": "2021",
            "doc": true,
            "doctest": true,
            "test": true
        }],
        "features": features.iter().map(|(k, v)| ((*k).to_string(), v.iter().map(|item| (*item).to_string()).collect::<Vec<_>>())).collect::<HashMap<_, _>>(),
        "manifest_path": format!("/tmp/{name}/Cargo.toml"),
        "metadata": metadata,
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
    });

    serde_json::from_value(value).unwrap()
}

pub(super) fn metadata_with_packages(
    packages: Vec<Package>,
    workspace_members: &[&str],
) -> Metadata {
    let package_refs = packages;
    let value = serde_json::json!({
        "packages": package_refs,
        "workspace_members": workspace_members,
        "workspace_default_members": workspace_members,
        "resolve": null,
        "target_directory": "/tmp/target",
        "version": 1,
        "workspace_root": "/tmp/ws",
        "metadata": null,
    });

    serde_json::from_value(value).unwrap()
}

pub(super) fn metadata_with_resolve(packages: Vec<Package>, deps: &[(&str, &[&str])]) -> Metadata {
    let members = packages
        .iter()
        .map(|package| package.id.repr.as_str())
        .collect::<Vec<_>>();
    let ids = packages
        .iter()
        .map(|package| (package.name.as_str(), package.id.repr.as_str()))
        .collect::<HashMap<_, _>>();
    let nodes = deps
        .iter()
        .map(|(name, deps)| {
            serde_json::json!({
                "id": ids[name],
                "dependencies": deps.iter().map(|dep| ids[dep]).collect::<Vec<_>>(),
                "deps": deps.iter().map(|dep| {
                    serde_json::json!({
                        "name": dep,
                        "pkg": ids[dep],
                        "dep_kinds": [{ "kind": null, "target": null }]
                    })
                }).collect::<Vec<_>>(),
                "features": []
            })
        })
        .collect::<Vec<_>>();
    let value = serde_json::json!({
        "packages": packages,
        "workspace_members": members,
        "workspace_default_members": members,
        "resolve": { "nodes": nodes, "root": null },
        "target_directory": "/tmp/target",
        "version": 1,
        "workspace_root": "/tmp/ws",
        "metadata": null,
    });

    serde_json::from_value(value).unwrap()
}

pub(super) fn metadata_for_packages(packages: &[Package]) -> Metadata {
    let members = packages
        .iter()
        .map(|package| package.id.repr.as_str())
        .collect::<Vec<_>>();
    metadata_with_packages(packages.to_vec(), &members)
}

pub(super) fn expand(packages: &[Package]) -> Vec<ClippyCheck> {
    let selected = packages
        .iter()
        .cloned()
        .map(|package| SelectedClippyPackage {
            package,
            deps_mode: ClippyDepsMode::NoDeps,
        })
        .collect::<Vec<_>>();
    expand_clippy_checks(&selected, &metadata_for_packages(packages))
        .expect("test package clippy checks should expand")
}

pub(super) fn args(all: bool, packages: &[&str]) -> crate::ClippyArgs {
    crate::ClippyArgs {
        all,
        packages: packages
            .iter()
            .map(|package| (*package).to_string())
            .collect(),
        since: None,
    }
}

pub(super) struct FakeCargoRunner {
    results: HashMap<ClippyCheck, bool>,
    pub(super) invocations: Vec<(PathBuf, ClippyCheck)>,
}

impl FakeCargoRunner {
    pub(super) fn new(results: &[(ClippyCheck, bool)]) -> Self {
        Self {
            results: results.iter().cloned().collect(),
            invocations: Vec::new(),
        }
    }
}

impl CargoRunner for FakeCargoRunner {
    fn run_clippy(&mut self, workspace_root: &Path, check: &ClippyCheck) -> anyhow::Result<bool> {
        self.invocations
            .push((workspace_root.to_path_buf(), check.clone()));
        Ok(*self.results.get(check).unwrap_or(&true))
    }
}
