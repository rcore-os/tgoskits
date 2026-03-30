use std::{
    collections::{BTreeSet, HashSet},
    path::Path,
    process::Command,
};

use anyhow::Context;
use cargo_metadata::{Metadata, MetadataCommand, Package};
use serde_json::Value;

pub(crate) fn run_workspace_clippy_command() -> anyhow::Result<()> {
    let metadata = MetadataCommand::new()
        .no_deps()
        .exec()
        .context("failed to load cargo metadata")?;
    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();
    let packages = workspace_packages(&metadata);
    let checks = expand_clippy_checks(&packages);

    println!(
        "running clippy for {} package(s) with {} check(s) from {}",
        packages.len(),
        checks.len(),
        workspace_root.display()
    );

    let mut runner = ProcessCargoRunner;
    run_clippy_checks(&mut runner, &workspace_root, &checks)
}

fn workspace_packages(metadata: &Metadata) -> Vec<Package> {
    let workspace_members: HashSet<_> = metadata.workspace_members.iter().cloned().collect();
    let mut packages: Vec<_> = metadata
        .packages
        .iter()
        .filter(|pkg| workspace_members.contains(&pkg.id))
        .cloned()
        .collect();
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    packages
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ClippyCheckKind {
    Base,
    Feature(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ClippyCheck {
    package: String,
    kind: ClippyCheckKind,
    target: Option<String>,
}

impl ClippyCheck {
    fn cargo_args(&self) -> Vec<String> {
        let mut args = match &self.kind {
            ClippyCheckKind::Base => vec!["clippy".into(), "-p".into(), self.package.clone()],
            ClippyCheckKind::Feature(feature) => vec![
                "clippy".into(),
                "-p".into(),
                self.package.clone(),
                "--no-default-features".into(),
                "--features".into(),
                feature.clone(),
            ],
        };
        if let Some(target) = &self.target {
            args.extend(["--target".into(), target.clone()]);
        }
        args.extend(["--".into(), "-D".into(), "warnings".into()]);
        args
    }

    fn label(&self) -> String {
        let base = match &self.kind {
            ClippyCheckKind::Base => format!("{} (base", self.package),
            ClippyCheckKind::Feature(feature) => {
                format!("{} (feature: {}", self.package, feature)
            }
        };

        match &self.target {
            Some(target) => format!("{base}, target: {target})"),
            None => format!("{base})"),
        }
    }
}

fn docs_rs_targets(package: &Package) -> Vec<String> {
    let Some(docs_rs) = package.metadata.get("docs.rs").and_then(Value::as_object) else {
        return Vec::new();
    };

    let Some(targets) = docs_rs.get("targets").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut unique_targets = BTreeSet::new();
    for target in targets.iter().filter_map(Value::as_str) {
        unique_targets.insert(target.to_string());
    }

    unique_targets.into_iter().collect()
}

fn expand_clippy_checks(packages: &[Package]) -> Vec<ClippyCheck> {
    let mut checks = Vec::new();

    for package in packages {
        let features: BTreeSet<_> = package
            .features
            .keys()
            .filter(|feature| feature.as_str() != "default")
            .cloned()
            .collect();
        let targets = docs_rs_targets(package);
        let target_iter = if targets.is_empty() {
            vec![None]
        } else {
            targets.into_iter().map(Some).collect()
        };

        for target in target_iter {
            checks.push(ClippyCheck {
                package: package.name.to_string(),
                kind: ClippyCheckKind::Base,
                target: target.clone(),
            });

            for feature in &features {
                checks.push(ClippyCheck {
                    package: package.name.to_string(),
                    kind: ClippyCheckKind::Feature(feature.clone()),
                    target: target.clone(),
                });
            }
        }
    }

    checks
}

fn run_clippy_checks<R: CargoRunner>(
    runner: &mut R,
    workspace_root: &Path,
    checks: &[ClippyCheck],
) -> anyhow::Result<()> {
    for (index, check) in checks.iter().enumerate() {
        let args = check.cargo_args();
        println!("[{}/{}] {}", index + 1, checks.len(), check.label());
        println!("          cargo {}", args.join(" "));

        if runner.run_clippy(workspace_root, check)? {
            println!("ok: {}", check.label());
            continue;
        }

        bail!("clippy failed for {}", check.label());
    }

    println!("all clippy checks passed");
    Ok(())
}

trait CargoRunner {
    fn run_clippy(&mut self, workspace_root: &Path, check: &ClippyCheck) -> anyhow::Result<bool>;
}

struct ProcessCargoRunner;

impl CargoRunner for ProcessCargoRunner {
    fn run_clippy(&mut self, workspace_root: &Path, check: &ClippyCheck) -> anyhow::Result<bool> {
        let args = check.cargo_args();
        let status = Command::new("cargo")
            .current_dir(workspace_root)
            .args(&args)
            .status()
            .with_context(|| format!("failed to spawn `cargo {}`", args.join(" ")))?;
        Ok(status.success())
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use super::*;

    fn pkg(
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

    fn metadata_with_packages(packages: Vec<Package>, workspace_members: &[&str]) -> Metadata {
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

    struct FakeCargoRunner {
        results: HashMap<ClippyCheck, bool>,
        invocations: Vec<(PathBuf, ClippyCheck)>,
    }

    impl FakeCargoRunner {
        fn new(results: &[(ClippyCheck, bool)]) -> Self {
            Self {
                results: results.iter().cloned().collect(),
                invocations: Vec::new(),
            }
        }
    }

    impl CargoRunner for FakeCargoRunner {
        fn run_clippy(
            &mut self,
            workspace_root: &Path,
            check: &ClippyCheck,
        ) -> anyhow::Result<bool> {
            self.invocations
                .push((workspace_root.to_path_buf(), check.clone()));
            Ok(*self.results.get(check).unwrap_or(&true))
        }
    }

    #[test]
    fn workspace_package_extraction_keeps_only_workspace_members() {
        let metadata = metadata_with_packages(
            vec![
                pkg("beta", "beta 0.1.0 (path+file:///tmp/beta)", &[], None),
                pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
                pkg("gamma", "gamma 0.1.0 (path+file:///tmp/gamma)", &[], None),
            ],
            &[
                "beta 0.1.0 (path+file:///tmp/beta)",
                "alpha 0.1.0 (path+file:///tmp/alpha)",
            ],
        );

        let packages = workspace_packages(&metadata);

        assert_eq!(
            packages
                .iter()
                .map(|pkg| pkg.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
    }

    #[test]
    fn feature_expansion_ignores_default() {
        let packages = vec![pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[("default", &["feat-a"]), ("feat-b", &[]), ("feat-a", &[])],
            None,
        )];

        let checks = expand_clippy_checks(&packages);

        assert_eq!(
            checks,
            vec![
                ClippyCheck {
                    package: "alpha".into(),
                    kind: ClippyCheckKind::Base,
                    target: None,
                },
                ClippyCheck {
                    package: "alpha".into(),
                    kind: ClippyCheckKind::Feature("feat-a".into()),
                    target: None,
                },
                ClippyCheck {
                    package: "alpha".into(),
                    kind: ClippyCheckKind::Feature("feat-b".into()),
                    target: None,
                },
            ]
        );
    }

    #[test]
    fn feature_expansion_is_deterministic() {
        let packages = vec![
            pkg(
                "beta",
                "beta 0.1.0 (path+file:///tmp/beta)",
                &[("zeta", &[]), ("alpha", &[])],
                None,
            ),
            pkg(
                "alpha",
                "alpha 0.1.0 (path+file:///tmp/alpha)",
                &[("middle", &[]), ("default", &[])],
                None,
            ),
        ];

        let checks = expand_clippy_checks(&packages);

        assert_eq!(
            checks
                .into_iter()
                .map(|check| check.label())
                .collect::<Vec<_>>(),
            vec![
                "beta (base)",
                "beta (feature: alpha)",
                "beta (feature: zeta)",
                "alpha (base)",
                "alpha (feature: middle)",
            ]
        );
    }

    #[test]
    fn package_without_features_yields_only_base_check() {
        let checks = expand_clippy_checks(&[pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[],
            None,
        )]);

        assert_eq!(
            checks,
            vec![ClippyCheck {
                package: "alpha".into(),
                kind: ClippyCheckKind::Base,
                target: None,
            }]
        );
    }

    #[test]
    fn package_with_features_yields_base_plus_each_feature() {
        let checks = expand_clippy_checks(&[pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[("b", &[]), ("a", &[])],
            None,
        )]);

        assert_eq!(checks.len(), 3);
        assert_eq!(
            checks[0].cargo_args(),
            vec!["clippy", "-p", "alpha", "--", "-D", "warnings"]
        );
        assert_eq!(
            checks[1].cargo_args(),
            vec![
                "clippy",
                "-p",
                "alpha",
                "--no-default-features",
                "--features",
                "a",
                "--",
                "-D",
                "warnings",
            ]
        );
        assert_eq!(
            checks[2].cargo_args(),
            vec![
                "clippy",
                "-p",
                "alpha",
                "--no-default-features",
                "--features",
                "b",
                "--",
                "-D",
                "warnings",
            ]
        );
    }

    #[test]
    fn docs_rs_targets_expand_base_and_feature_checks() {
        let checks = expand_clippy_checks(&[pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[("b", &[]), ("a", &[])],
            Some(&["riscv64gc-unknown-none-elf"]),
        )]);

        assert_eq!(checks.len(), 3);
        assert_eq!(
            checks[0].cargo_args(),
            vec![
                "clippy",
                "-p",
                "alpha",
                "--target",
                "riscv64gc-unknown-none-elf",
                "--",
                "-D",
                "warnings",
            ]
        );
        assert_eq!(
            checks[1].cargo_args(),
            vec![
                "clippy",
                "-p",
                "alpha",
                "--no-default-features",
                "--features",
                "a",
                "--target",
                "riscv64gc-unknown-none-elf",
                "--",
                "-D",
                "warnings",
            ]
        );
        assert_eq!(
            checks[2].label(),
            "alpha (feature: b, target: riscv64gc-unknown-none-elf)"
        );
    }

    #[test]
    fn docs_rs_targets_are_sorted_and_deduplicated() {
        let checks = expand_clippy_checks(&[pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[("feat", &[])],
            Some(&[
                "x86_64-unknown-none",
                "aarch64-unknown-none-softfloat",
                "x86_64-unknown-none",
            ]),
        )]);

        assert_eq!(
            checks
                .into_iter()
                .map(|check| check.label())
                .collect::<Vec<_>>(),
            vec![
                "alpha (base, target: aarch64-unknown-none-softfloat)",
                "alpha (feature: feat, target: aarch64-unknown-none-softfloat)",
                "alpha (base, target: x86_64-unknown-none)",
                "alpha (feature: feat, target: x86_64-unknown-none)",
            ]
        );
    }

    #[test]
    fn empty_docs_rs_targets_fall_back_to_host_clippy() {
        let package = pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[],
            Some(&[]),
        );

        assert!(docs_rs_targets(&package).is_empty());
        assert_eq!(
            expand_clippy_checks(&[package])[0].cargo_args(),
            vec!["clippy", "-p", "alpha", "--", "-D", "warnings"]
        );
    }

    #[test]
    fn fail_fast_runner_stops_after_first_failed_invocation() {
        let root = PathBuf::from("/tmp/workspace");
        let checks = vec![
            ClippyCheck {
                package: "alpha".into(),
                kind: ClippyCheckKind::Base,
                target: None,
            },
            ClippyCheck {
                package: "alpha".into(),
                kind: ClippyCheckKind::Feature("feat-a".into()),
                target: None,
            },
            ClippyCheck {
                package: "beta".into(),
                kind: ClippyCheckKind::Base,
                target: None,
            },
        ];
        let mut runner = FakeCargoRunner::new(&[
            (checks[0].clone(), true),
            (checks[1].clone(), false),
            (checks[2].clone(), true),
        ]);

        let err = run_clippy_checks(&mut runner, &root, &checks).unwrap_err();

        assert!(err.to_string().contains("alpha (feature: feat-a)"));
        assert_eq!(
            runner.invocations,
            vec![(root.clone(), checks[0].clone()), (root, checks[1].clone()),]
        );
    }
}
