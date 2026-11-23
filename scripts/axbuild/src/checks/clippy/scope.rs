//! Incremental selection facts and conservative workspace consumer closure.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{Context, ensure};
use cargo_metadata::{Metadata, Package};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    check::ClippyCheck,
    expand::expand_clippy_checks,
    plan,
    selection::{clippy_skip_reason, resolve_requested_packages, validate_package_coverage},
};
use crate::support::git::{self, IncrementalPackageSelection};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Mode {
    Full,
    Incremental,
    Empty,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Scope {
    pub schema_version: u32,
    pub selection_rules_version: u32,
    pub mode: Mode,
    pub reason_code: String,
    pub requested_since: Option<String>,
    pub diff_base_sha: Option<String>,
    pub tested_sha: Option<String>,
    pub input_fingerprint: String,
    pub selected_packages: Vec<String>,
    pub changed: Vec<String>,
    pub affected: Vec<String>,
    // No test-only pruning is enabled without audited input isolation.
    pub cut: Vec<String>,
    pub checks: Vec<ClippyCheck>,
    pub selection_digest: String,
}

pub(super) fn select(
    args: &crate::ClippyArgs,
    root: &Path,
    metadata: &Metadata,
    all: &[Package],
) -> anyhow::Result<Scope> {
    let tested_sha = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    ensure!(
        !args.print_selection || args.since.is_none() || tested_sha.is_some(),
        "--print-selection --since requires a Git working tree"
    );
    let mut mode = Mode::Full;
    let mut reason = "explicit_full";
    let mut base = None;
    let mut changed = Vec::new();
    let mut affected = Vec::new();
    let selected = if let Some(since) = &args.since {
        let resolved = if let Some(fixed) = &args.diff_base {
            let resolved = git::resolve_since_diff_base(root, fixed)?;
            ensure!(
                &resolved == fixed,
                "fixed Clippy diff base is no longer an ancestor"
            );
            Ok(resolved)
        } else {
            git::resolve_since_diff_base(root, since)
        };
        match resolved {
            Ok(resolved) => {
                base = Some(resolved.clone());
                match git::select_incremental_packages(root, metadata, all, &resolved)? {
                    IncrementalPackageSelection::Packages {
                        changed: direct,
                        affected: existing,
                    } => {
                        changed = direct;
                        match consumer_closure(all, &changed, &existing) {
                            Ok(consumers) => {
                                affected = consumers;
                                mode = if affected.is_empty() {
                                    Mode::Empty
                                } else {
                                    Mode::Incremental
                                };
                                reason = if affected.is_empty() {
                                    "legacy_no_relevant_packages"
                                } else {
                                    "workspace_consumers"
                                };
                                affected.clone()
                            }
                            Err(error) => {
                                eprintln!(
                                    "Clippy consumer analysis uncertain: {error:#}; using full \
                                     workspace"
                                );
                                reason = "consumer_analysis_unavailable";
                                all.iter().map(|p| p.name.to_string()).collect()
                            }
                        }
                    }
                    IncrementalPackageSelection::Full { reason: detail } => {
                        eprintln!("Clippy full fallback: {detail}");
                        reason = "legacy_full_fallback";
                        all.iter().map(|p| p.name.to_string()).collect()
                    }
                }
            }
            Err(error) => {
                eprintln!("Clippy comparison unavailable: {error:#}; using full workspace");
                reason = "comparison_unavailable";
                all.iter().map(|p| p.name.to_string()).collect()
            }
        }
    } else {
        let packages = resolve_requested_packages(args, root, metadata, all)?;
        if !args.packages.is_empty() {
            mode = Mode::Incremental;
            reason = "explicit_packages";
        }
        packages.into_iter().map(|p| p.name.to_string()).collect()
    };
    let selected_set = selected.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let packages = all
        .iter()
        .filter(|p| selected_set.contains(p.name.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let supported = packages
        .iter()
        .filter(|p| clippy_skip_reason(p).is_none())
        .cloned()
        .collect::<Vec<_>>();
    for p in &packages {
        if let Some(reason) = clippy_skip_reason(p) {
            eprintln!("Clippy excluded {}: {reason}", p.name);
        }
    }
    let checks = expand_clippy_checks(&supported, metadata)?;
    validate_package_coverage(&packages, &checks)?;
    if checks.is_empty() && mode == Mode::Incremental {
        mode = Mode::Empty;
        reason = "only_explicitly_excluded_packages";
    }
    let fingerprint = plan::input_fingerprint(root, &checks)?;
    let mut scope = Scope {
        schema_version: 1,
        selection_rules_version: 1,
        mode,
        reason_code: reason.into(),
        requested_since: args.since.clone(),
        diff_base_sha: base,
        tested_sha,
        input_fingerprint: fingerprint,
        selected_packages: supported.iter().map(|p| p.name.to_string()).collect(),
        changed,
        affected,
        cut: Vec::new(),
        checks,
        selection_digest: String::new(),
    };
    scope.selection_digest = scope.digest(root)?;
    if let Some(expected) = &args.expected_selection {
        ensure!(
            scope.mode != Mode::Full,
            "incremental execution unexpectedly became full; re-plan"
        );
        ensure!(
            &scope.selection_digest == expected,
            "Clippy selection changed since planning"
        );
    }
    Ok(scope)
}

impl Scope {
    pub(super) fn digest(&self, root: &Path) -> anyhow::Result<String> {
        let mut document = serde_json::to_value(self)?;
        document
            .as_object_mut()
            .context("selection object")?
            .remove("selection_digest");
        normalize_paths(&mut document, &root.to_string_lossy());
        for key in ["selected_packages", "changed", "affected", "cut", "checks"] {
            if let Some(array) = document[key].as_array_mut() {
                array.sort_by_cached_key(serde_json::Value::to_string);
            }
        }
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&document)?)
        ))
    }
}

fn normalize_paths(value: &mut serde_json::Value, root: &str) {
    match value {
        serde_json::Value::String(s) => {
            if s == root {
                *s = "${workspace}".into();
            } else if let Some(tail) = s.strip_prefix(root).filter(|t| t.starts_with('/')) {
                *s = format!("${{workspace}}{tail}");
            } else if let Some(tail) = s.strip_prefix(&format!("-L{root}/")) {
                *s = format!("-L${{workspace}}/{tail}");
            }
        }
        serde_json::Value::Array(a) => {
            for item in a {
                normalize_paths(item, root);
            }
        }
        serde_json::Value::Object(o) => {
            for item in o.values_mut() {
                normalize_paths(item, root);
            }
        }
        _ => {}
    }
}

/// Union resolved consumers with every declared local workspace dependency,
/// including disabled optional and target-specific edges. Never guess by alias.
pub(super) fn consumer_closure(
    packages: &[Package],
    changed: &[String],
    resolved: &[String],
) -> anyhow::Result<Vec<String>> {
    let mut reverse = BTreeMap::<String, BTreeSet<String>>::new();
    for consumer in packages {
        for dep in &consumer.dependencies {
            let candidates = packages
                .iter()
                .filter(|p| p.name == dep.name && dep.req.matches(&p.version));
            for dependency in candidates {
                if let Some(path) = &dep.path {
                    ensure!(
                        path.as_std_path().canonicalize()?
                            == dependency
                                .manifest_path
                                .parent()
                                .context("manifest directory")?
                                .as_std_path()
                                .canonicalize()?,
                        "ambiguous local workspace dependency {}",
                        dep.name
                    );
                }
                reverse
                    .entry(dependency.name.to_string())
                    .or_default()
                    .insert(consumer.name.to_string());
            }
        }
    }
    let mut pending =
        super::selection::incremental_clippy_selections(changed.to_vec(), resolved.to_vec());
    let mut seen = BTreeSet::new();
    while let Some(p) = pending.pop() {
        if seen.insert(p.clone())
            && let Some(consumers) = reverse.get(&p)
        {
            pending.extend(consumers.iter().cloned());
        }
    }
    Ok(seen.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{super::check::ClippyCheckKind, *};

    #[test]
    fn declared_optional_target_build_and_dev_consumers_are_not_filtered() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers=[\"a\",\"b\",\"c\",\"d\"]\nresolver=\"3\"\n",
        )
        .unwrap();
        for (name, deps) in [
            ("a", ""),
            (
                "b",
                "[dependencies]\nrenamed={package=\"a\",path=\"../a\",optional=true}\n",
            ),
            (
                "c",
                "[target.'cfg(target_os = \"none\")'.build-dependencies]\nb={path=\"../b\"}\n",
            ),
            ("d", "[dev-dependencies]\nc={path=\"../c\"}\n"),
        ] {
            let path = root.path().join(name);
            fs::create_dir_all(path.join("src")).unwrap();
            fs::write(
                path.join("Cargo.toml"),
                format!("[package]\nname=\"{name}\"\nversion=\"0.1.0\"\nedition=\"2024\"\n{deps}"),
            )
            .unwrap();
            fs::write(path.join("src/lib.rs"), "").unwrap();
        }
        let metadata = cargo_metadata::MetadataCommand::new()
            .manifest_path(root.path().join("Cargo.toml"))
            .no_deps()
            .exec()
            .unwrap();
        assert!(metadata.resolve.is_none());
        let result = consumer_closure(&metadata.packages, &["a".into()], &[]).unwrap();
        assert_eq!(result, vec!["a", "b", "c", "d"]);
        assert_eq!(
            consumer_closure(&metadata.packages, &["d".into()], &[]).unwrap(),
            vec!["d"]
        );
        assert_eq!(
            consumer_closure(&metadata.packages, &[], &["c".into()]).unwrap(),
            vec!["c", "d"]
        );
    }

    fn fixture_scope(root: &str) -> Scope {
        Scope {
            schema_version: 1,
            selection_rules_version: 1,
            mode: Mode::Incremental,
            reason_code: "fixture".into(),
            requested_since: Some("base".into()),
            diff_base_sha: Some("base".into()),
            tested_sha: Some("head".into()),
            input_fingerprint: "inputs".into(),
            selected_packages: vec!["b".into(), "a".into()],
            changed: vec!["a".into()],
            affected: vec!["a".into(), "b".into()],
            cut: vec![],
            checks: vec![ClippyCheck {
                package: "a".into(),
                kind: ClippyCheckKind::Configuration {
                    name: "fixture".into(),
                    features: vec![],
                    rustflags: vec!["-Copt-level=1".into(), "-Cdebuginfo=2".into()],
                },
                target: None,
                env: vec![("ASSET".into(), format!("{root}/asset"))],
            }],
            selection_digest: String::new(),
        }
    }

    #[test]
    fn selection_digest_is_canonical_but_preserves_ordered_compiler_arguments() {
        let root = Path::new("/workspace");
        let mut a = fixture_scope("/workspace");
        let original = a.digest(root).unwrap();
        a.selected_packages.reverse();
        a.affected.reverse();
        assert_eq!(original, a.digest(root).unwrap());
        assert_eq!(
            original,
            fixture_scope("/other").digest(Path::new("/other")).unwrap()
        );
        a.tested_sha = Some("changed".into());
        assert_ne!(original, a.digest(root).unwrap());
        a = fixture_scope("/workspace");
        a.diff_base_sha = Some("other".into());
        assert_ne!(original, a.digest(root).unwrap());
        a = fixture_scope("/workspace");
        if let ClippyCheckKind::Configuration { rustflags, .. } = &mut a.checks[0].kind {
            rustflags.reverse();
        }
        assert_ne!(original, a.digest(root).unwrap());
        let mut version_changed = fixture_scope("/workspace");
        version_changed.selection_rules_version += 1;
        assert_ne!(original, version_changed.digest(root).unwrap());
        let mut value = serde_json::json!([
            "/workspace-sibling/asset",
            "/workspace/asset",
            "-L/workspace/native"
        ]);
        normalize_paths(&mut value, "/workspace");
        assert_eq!(
            value,
            serde_json::json!([
                "/workspace-sibling/asset",
                "${workspace}/asset",
                "-L${workspace}/native"
            ])
        );
        for mode in [Mode::Full, Mode::Incremental, Mode::Empty] {
            let mut scope = fixture_scope("/workspace");
            scope.mode = mode;
            if mode != Mode::Incremental {
                scope.selected_packages.clear();
                scope.checks.clear();
            }
            if mode == Mode::Full {
                scope.requested_since = None;
                scope.diff_base_sha = None;
            }
            let line = serde_json::to_string(&scope).unwrap();
            assert!(!line.contains('\n'));
            let restored: Scope = serde_json::from_str(&line).unwrap();
            assert_eq!(restored.mode, mode);
            let mut invalid: serde_json::Value = serde_json::from_str(&line).unwrap();
            invalid["unknown"] = true.into();
            assert!(serde_json::from_value::<Scope>(invalid).is_err());
        }
    }
}
