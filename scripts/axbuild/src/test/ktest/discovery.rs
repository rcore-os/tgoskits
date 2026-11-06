use super::*;

#[derive(Deserialize)]
struct AxtestTargetFlagsWire {
    #[serde(default)]
    board_only: bool,
}


#[derive(Deserialize)]
struct AxtestMetadataWire {
    #[serde(default)]
    runtime: Option<String>,
    #[serde(default)]
    target_triples: Option<Vec<String>>,
    #[serde(default)]
    targets: BTreeMap<String, AxtestTargetFlagsWire>,
}


/// Typed, validated view of the metadata ktest owns for one package.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct AxtestMetadata {
    runtime: Option<KtestRuntime>,
    /// Kernel-test target triples, including the docs.rs fallback.
    test_targets: Option<Vec<String>>,
    /// Per-target flags keyed by Cargo test target name.
    board_only: BTreeMap<String, bool>,
}


pub(super) fn is_harness_false_test(target: &KtestTarget) -> bool {
    target.kind == KtestTargetKind::Test && !target.harness
}


pub(super) fn load_discovered_ktest_package(
    metadata: &Metadata,
    package: &str,
) -> anyhow::Result<DiscoveredKtestPackage> {
    discover_workspace_ktests(metadata)?
        .into_iter()
        .find(|candidate| candidate.name == package)
        .ok_or_else(|| anyhow!("workspace package `{package}` not found"))
}


pub(super) fn discover_workspace_ktests(metadata: &Metadata) -> anyhow::Result<Vec<DiscoveredKtestPackage>> {
    let workspace_members = metadata
        .workspace_members
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    let axtest = metadata
        .packages
        .iter()
        .find(|package| {
            package.name.as_str() == AXTEST_FEATURE && workspace_members.contains(&package.id)
        })
        .ok_or_else(|| anyhow!("workspace package `axtest` not found"))?;
    let axtest_dir = axtest
        .manifest_path
        .parent()
        .ok_or_else(|| anyhow!("invalid axtest manifest path {}", axtest.manifest_path))?;
    let mut packages = Vec::new();

    for package in metadata
        .packages
        .iter()
        .filter(|package| workspace_members.contains(&package.id))
    {
        let uses_workspace_axtest = package.dependencies.iter().any(|dependency| {
            dependency.name == AXTEST_FEATURE && dependency.path.as_deref() == Some(axtest_dir)
        });
        let axtest = axtest_metadata(&package.name, &package.metadata)?;
        let ktest_package = ktest_package_from_metadata(package, &axtest)?;
        packages.push(DiscoveredKtestPackage {
            name: ktest_package.name,
            manifest_path: package.manifest_path.as_std_path().to_path_buf(),
            uses_workspace_axtest,
            runtime: axtest.runtime.unwrap_or(KtestRuntime::Arceos),
            targets: ktest_package.targets,
            test_targets: axtest.test_targets,
        });
    }
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(packages)
}


pub(super) fn axtest_metadata(package: &str, metadata: &serde_json::Value) -> anyhow::Result<AxtestMetadata> {
    let wire: AxtestMetadataWire = match metadata.get("axtest") {
        Some(axtest) => serde_json::from_value(axtest.clone()).with_context(|| {
            format!("package `{package}` has an invalid [package.metadata.axtest] table")
        })?,
        None => AxtestMetadataWire {
            runtime: None,
            target_triples: None,
            targets: BTreeMap::new(),
        },
    };
    let board_only = wire
        .targets
        .into_iter()
        .map(|(name, flags)| (name, flags.board_only))
        .collect();
    let runtime = match wire.runtime.as_deref() {
        Some(runtime) => Some(runtime_from_name(package, runtime)?),
        None => None,
    };
    let test_targets = match wire.target_triples {
        Some(targets) => Some(targets),
        None => docs_rs_targets(package, metadata)?,
    };
    Ok(AxtestMetadata {
        runtime,
        test_targets,
        board_only,
    })
}


pub(super) fn runtime_from_name(package: &str, runtime: &str) -> anyhow::Result<KtestRuntime> {
    match runtime {
        "arceos" => Ok(KtestRuntime::Arceos),
        "starry" => Ok(KtestRuntime::Starry),
        "axvisor" => Ok(KtestRuntime::Axvisor),
        "board" => Ok(KtestRuntime::Board),
        _ => bail!(
            "package `{package}` has unsupported [package.metadata.axtest] runtime `{runtime}`"
        ),
    }
}


pub(super) fn docs_rs_targets(
    package: &str,
    metadata: &serde_json::Value,
) -> anyhow::Result<Option<Vec<String>>> {
    let Some(targets) = metadata
        .get("docs")
        .and_then(|docs| docs.get("rs"))
        .and_then(|docs_rs| docs_rs.get("targets"))
    else {
        return Ok(None);
    };
    let targets = targets.as_array().ok_or_else(|| {
        anyhow!("package `{package}` must declare [package.metadata.docs.rs].targets as an array")
    })?;
    targets
        .iter()
        .map(|target| {
            target
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow!("package `{package}` has a non-string docs.rs target"))
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .map(Some)
}


pub(super) fn ktest_package_from_metadata(
    package: &Package,
    axtest: &AxtestMetadata,
) -> anyhow::Result<KtestPackage> {
    let harness = test_harness_flags(package.manifest_path.as_std_path())?;
    let targets = package
        .targets
        .iter()
        .map(|target| {
            let kind = if target.is_test() {
                KtestTargetKind::Test
            } else if target.is_lib() {
                KtestTargetKind::Lib
            } else {
                KtestTargetKind::Other
            };
            KtestTarget {
                name: target.name.clone(),
                kind,
                harness: harness.get(&target.name).copied().unwrap_or(true),
                required_features: target.required_features.clone(),
                board_only: axtest
                    .board_only
                    .get(&target.name)
                    .copied()
                    .unwrap_or(false),
            }
        })
        .collect();
    Ok(KtestPackage {
        name: package.name.to_string(),
        targets,
    })
}


pub(super) fn test_harness_flags(manifest_path: &Path) -> anyhow::Result<HashMap<String, bool>> {
    let content = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let value: toml::Value = toml::from_str(&content)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    let mut flags = HashMap::new();
    if let Some(tests) = value.get("test").and_then(toml::Value::as_array) {
        for entry in tests {
            let Some(table) = entry.as_table() else {
                continue;
            };
            let Some(name) = table.get("name").and_then(toml::Value::as_str) else {
                continue;
            };
            let harness = table
                .get("harness")
                .and_then(toml::Value::as_bool)
                .unwrap_or(true);
            flags.insert(name.to_string(), harness);
        }
    }
    Ok(flags)
}


pub(super) fn select_ktest_target<'a>(
    package: &'a KtestPackage,
    explicit: Option<&str>,
) -> anyhow::Result<&'a KtestTarget> {
    if let Some(name) = explicit {
        let target = package
            .targets
            .iter()
            .find(|target| target.name == name)
            .ok_or_else(|| {
                anyhow!(
                    "test target `{name}` not found in package `{}`",
                    package.name
                )
            })?;
        if target.kind != KtestTargetKind::Test || target.harness {
            bail!(
                "test target `{}` in package `{}` must be a harness=false [[test]] target",
                target.name,
                package.name
            );
        }
        return Ok(target);
    }

    let candidates = package
        .targets
        .iter()
        .filter(|target| is_harness_false_test(target))
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [target] => Ok(*target),
        [] => bail!(
            "package `{}` has no harness=false [[test]] target; pass --test after adding one",
            package.name
        ),
        many => {
            let names = many
                .iter()
                .map(|target| target.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "package `{}` has multiple harness=false [[test]] targets ({names}); pass --test",
                package.name
            )
        }
    }
}

