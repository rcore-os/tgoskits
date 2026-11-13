//! Read-only Rust suite selection and startup planning.

use std::{collections::BTreeSet, fs, path::Path};

use anyhow::{Context, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{
    super::{
        assets::arceos_rust_test_dir, discovery::discover_rust_qemu_cases, types::ArceosQemuCase,
    },
    SuiteCatalog, load_catalog,
    profile::apply_rust_qemu_feature_overrides,
};
use crate::arceos::ArceOS;

#[derive(Debug, Serialize)]
struct SuitePlan {
    arch: String,
    target: String,
    logical_tests: usize,
    test_invocations: usize,
    build_identities: usize,
    planned_boots: usize,
    host_cpu_affinity: Option<String>,
    runs: Vec<RunPlan>,
}

#[derive(Debug, Serialize)]
struct RunPlan {
    profile: String,
    build_identity: String,
    build_config: String,
    qemu_config: String,
    cargo_features: Vec<String>,
    build_configuration: toml::Value,
    runtime_configuration: toml::Value,
    runtime_checks: serde_json::Value,
    host_fixtures: Vec<&'static str>,
    guest_cpus: usize,
    tests: Vec<PlannedTest>,
}

#[derive(Debug, Serialize)]
struct PlannedTest {
    id: String,
    name: String,
    category: String,
    entry: String,
}

pub(crate) fn print_plan(
    arceos: &ArceOS,
    arch: &str,
    target: &str,
    selected: Option<&str>,
) -> anyhow::Result<()> {
    let root = arceos_rust_test_dir(arceos);
    let manifest = read_config(&root.join("Cargo.toml"))?;
    let features = manifest
        .get("features")
        .context("suite has no feature graph")?;
    let catalog = load_catalog()?;
    let axstd_features = arceos
        .app
        .workspace_context()
        .metadata()
        .packages
        .iter()
        .find(|package| package.name == "ax-std")
        .context("suite std provider is missing from workspace metadata")?
        .features
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let cases = discover_rust_qemu_cases(arceos, arch, target, selected, false, &catalog)?;
    let runs = cases
        .iter()
        .map(|case| plan_run(case, arch, target, features, &catalog, &axstd_features))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let (logical_tests, test_invocations, build_identities) = counts(&runs);
    let plan = SuitePlan {
        arch: arch.to_owned(),
        target: target.to_owned(),
        logical_tests,
        test_invocations,
        build_identities,
        planned_boots: runs.len(),
        host_cpu_affinity: host_cpu_affinity(),
        runs,
    };
    println!("{}", serde_json::to_string_pretty(&plan)?);
    Ok(())
}

fn plan_run(
    case: &ArceosQemuCase,
    arch: &str,
    target: &str,
    graph: &toml::Value,
    catalog: &SuiteCatalog,
    axstd_features: &[String],
) -> anyhow::Result<RunPlan> {
    let profile = case
        .feature
        .as_deref()
        .context("Rust profile has no feature")?;
    let build = read_config(&case.build_config_path)?;
    let runtime = read_config(&case.case.qemu_config_path)?;
    let mut build_config: crate::arceos::config::ArceosConfig = build.clone().try_into()?;
    crate::build::validate_features(&build_config.common)?;
    crate::build::apply_makefile_features(
        &mut build_config.common,
        &crate::build::makefile_features_from_env(),
    )?;
    let app_features = graph
        .as_table()
        .context("feature graph must be a table")?
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let mut cargo_features = crate::build::resolved_std_build_features(
        &build_config.common,
        &app_features,
        axstd_features,
    )?;
    cargo_features.extend(case.features.iter().cloned());
    cargo_features.sort();
    cargo_features.dedup();
    let mut roots = cargo_features.clone();
    if graph.get("default").is_some() {
        roots.push("default".to_owned());
    }
    let enabled = expand_features(graph, &roots)?;
    let tests = catalog
        .tests()
        .filter(|test| {
            enabled.contains(&test.feature) && catalog.supports_test(&test.feature, arch)
        })
        .map(|test| PlannedTest {
            id: test.feature.clone(),
            name: test.name.clone(),
            category: test.category.clone(),
            entry: test.entry.clone(),
        })
        .collect::<Vec<_>>();
    if tests.is_empty() {
        bail!("Rust profile `{profile}` selects no target tests");
    }
    build_config.common.validate()?;
    let guest_cpus = build_config.common.max_cpu_num.unwrap_or(1);
    let build_identity = serde_json::to_vec(&(
        &case.package,
        target,
        case.build_config_path
            .strip_prefix(&case.case.case_dir)
            .unwrap_or(&case.build_config_path),
        &cargo_features,
        &build,
    ))?;
    let mut checks: ostool::run::qemu::QemuConfig = runtime.clone().try_into()?;
    crate::execution::qemu::apply_smp_qemu_arg(&mut checks, Some(guest_cpus));
    apply_rust_qemu_feature_overrides(&mut checks, Some(profile));
    crate::execution::qemu::apply_timeout_scale(&mut checks);
    let mut host_fixtures = Vec::new();
    if case.case.host_http_server.is_some() {
        host_fixtures.push("http-server");
    }
    if profile == "serial-rx" {
        host_fixtures.push("serial-rx");
    }
    if crate::execution::host_udp::HostUdpEchoGuard::is_configured(&runtime)? {
        host_fixtures.push("udp-echo");
    }
    if runtime.get("host_pcm_waveform").is_some() {
        host_fixtures.push("pcm-waveform");
    }
    Ok(RunPlan {
        profile: profile.to_owned(),
        build_identity: format!("{:x}", Sha256::digest(&build_identity)),
        build_config: case.build_config_path.display().to_string(),
        qemu_config: case.case.qemu_config_path.display().to_string(),
        cargo_features,
        build_configuration: build,
        runtime_configuration: runtime,
        runtime_checks: serde_json::to_value(checks)?,
        host_fixtures,
        guest_cpus,
        tests,
    })
}

fn read_config(path: &Path) -> anyhow::Result<toml::Value> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("failed to read plan configuration {}", path.display()))?;
    toml::from_str(&source)
        .with_context(|| format!("failed to parse plan configuration {}", path.display()))
}

fn expand_features(graph: &toml::Value, roots: &[String]) -> anyhow::Result<BTreeSet<String>> {
    let graph = graph.as_table().context("feature graph must be a table")?;
    let mut enabled = BTreeSet::new();
    let mut pending = roots.to_vec();
    while let Some(feature) = pending.pop() {
        if feature.starts_with("dep:") || feature.contains('/') || !enabled.insert(feature.clone())
        {
            continue;
        }
        let dependencies = graph
            .get(&feature)
            .with_context(|| format!("unknown suite feature `{feature}`"))?
            .as_array()
            .context("feature dependencies must be an array")?;
        for dependency in dependencies {
            pending.push(
                dependency
                    .as_str()
                    .context("feature dependency must be a string")?
                    .to_owned(),
            );
        }
    }
    Ok(enabled)
}

fn counts(runs: &[RunPlan]) -> (usize, usize, usize) {
    let tests: BTreeSet<_> = runs
        .iter()
        .flat_map(|run| run.tests.iter().map(|test| &test.id))
        .collect();
    let builds: BTreeSet<_> = runs.iter().map(|run| &run.build_identity).collect();
    (
        tests.len(),
        runs.iter().map(|run| run.tests.len()).sum(),
        builds.len(),
    )
}

fn host_cpu_affinity() -> Option<String> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("Cpus_allowed_list:")
                .map(str::trim)
                .map(str::to_owned)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_expansion_handles_shared_dependencies_cycles_and_invalid_roots() {
        let graph: toml::Value = toml::from_str(
            "batch = ['first', 'second']\nfirst = ['shared']\nsecond = ['shared']\nshared = \
             ['first', 'dep:driver', 'driver/queue']\n",
        )
        .unwrap();
        assert_eq!(
            expand_features(&graph, &["batch".into()]).unwrap(),
            ["batch", "first", "second", "shared"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert!(expand_features(&graph, &["missing".into()]).is_err());
    }

    #[test]
    fn planning_reads_configuration_without_preparing_referenced_assets() {
        let dir = tempfile::tempdir().unwrap();
        let build_config_path = dir.path().join("build.toml");
        let qemu_config_path = dir.path().join("qemu.toml");
        fs::write(
            &build_config_path,
            "features = []\nmax_cpu_num = 4\nlog = \"Info\"\n",
        )
        .unwrap();
        fs::write(
            &qemu_config_path,
            "args = ['-drive', 'file=does-not-exist.img']\nuefi = false\nfail_regex = []\ntimeout \
             = 1\n",
        )
        .unwrap();
        let graph: toml::Value = toml::from_str("sample = []\n").unwrap();
        let catalog = super::super::catalog::parse_catalog(
            r#"schema_version=3
run_order=['one-boot']
[batches.one-boot]
[[batches.one-boot.tests]]
id='sample'
name='operation'
entry='crate::example::run'
category='example'
"#,
        )
        .unwrap();
        let case = ArceosQemuCase {
            case: crate::execution::case::TestQemuCase {
                name: "sample".into(),
                display_name: "sample".into(),
                case_dir: dir.path().into(),
                qemu_config_path,
                test_commands: vec![],
                grouped_command_selection: Default::default(),
                host_http_server: None,
                subcases: vec![],
                grouped_subcase_filter: None,
                ltp_case_id: None,
            },
            build_group: "program".into(),
            build_config_path,
            package: "program".into(),
            feature: Some("sample".into()),
            features: vec!["sample".into()],
        };
        let one = plan_run(
            &case,
            "riscv64",
            "target",
            &graph,
            &catalog,
            &["smp".into()],
        )
        .unwrap();
        let two = plan_run(
            &case,
            "riscv64",
            "target",
            &graph,
            &catalog,
            &["smp".into()],
        )
        .unwrap();
        assert_eq!(one.cargo_features, ["ax-std/smp", "sample"]);
        assert_eq!(counts(&[one, two]), (1, 2, 1));
        assert!(!dir.path().join("does-not-exist.img").exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
