use std::collections::BTreeSet;

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::catalog::{Catalog, Mode, Test, root};
use crate::{arceos::ArceOS, execution::qemu::ListedQemuCase};

pub(crate) fn listed_cases(
    arceos: &ArceOS,
    selected: Option<&str>,
) -> anyhow::Result<Vec<ListedQemuCase>> {
    let catalog = Catalog::load(&root(arceos.app.workspace_root()))?;
    let mut cases = Vec::new();
    for arch in crate::context::supported_arches() {
        let selections = match catalog.select(arch, selected) {
            Ok(selections) => selections,
            Err(_) if selected.is_some() => continue,
            Err(error) => return Err(error),
        };
        cases.extend(selections.into_iter().map(|selection| ListedQemuCase {
            name: selection.label(),
            archs: vec![arch.to_owned()],
        }));
    }
    anyhow::ensure!(!cases.is_empty(), "no CPU tests match {selected:?}");
    Ok(cases)
}

pub(crate) fn render_listing(
    arceos: &ArceOS,
    arch: Option<&str>,
    selected: Option<&str>,
) -> anyhow::Result<String> {
    let catalog = Catalog::load(&root(arceos.app.workspace_root()))?;
    let mut lines = vec!["cpu".to_owned()];
    let mut boots = 0;
    for current in crate::context::supported_arches() {
        if arch.is_some_and(|arch| arch != current) {
            continue;
        }
        let selections = match catalog.select(current, selected) {
            Ok(selections) => selections,
            Err(_) if arch.is_none() && selected.is_some() => continue,
            Err(error) => return Err(error),
        };
        for selection in selections {
            boots += 1;
            lines.push(format!(
                "  {boots}. {} [{} {:?} {:?}] tests={} boots=1",
                selection.label(),
                current,
                selection.batch.image,
                selection.batch.mode,
                selection.tests.len()
            ));
            lines.extend(
                selection
                    .tests
                    .iter()
                    .map(|test| format!("     {}: {} -> {}", test.id, test.name, test.entry)),
            );
        }
    }
    anyhow::ensure!(boots > 0, "no CPU tests match {selected:?}");
    lines.push(format!("total boots: {boots}"));
    Ok(lines.join("\n"))
}

#[derive(Serialize)]
struct Plan<'a> {
    arch: &'a str,
    target: &'a str,
    planned_boots: usize,
    logical_tests: usize,
    test_invocations: usize,
    build_identities: usize,
    runs: Vec<Run<'a>>,
}
#[derive(Serialize)]
struct Run<'a> {
    batch: String,
    package: &'a str,
    mode: Mode,
    build_config: String,
    qemu_config: String,
    cargo_features: Vec<String>,
    build_environment: std::collections::BTreeMap<String, String>,
    entry_capabilities: Vec<String>,
    runtime_features: Vec<String>,
    build_identity: String,
    build_configuration: toml::Value,
    runtime_configuration: toml::Value,
    tests: Vec<&'a Test>,
}

pub(crate) fn print_plan(
    arceos: &ArceOS,
    arch: &str,
    target: &str,
    selected: Option<&str>,
) -> anyhow::Result<()> {
    let root = root(arceos.app.workspace_root());
    let catalog = Catalog::load(&root)?;
    let selections = catalog.select(arch, selected)?;
    let mut runs = Vec::new();
    for selection in &selections {
        let batch = selection.batch;
        let build_config = root.join(&batch.build_config);
        let qemu_config = root.join(&batch.qemu_config);
        let build: toml::Value = toml::from_str(&std::fs::read_to_string(&build_config)?)?;
        let runtime: toml::Value = toml::from_str(&std::fs::read_to_string(&qemu_config)?)?;
        let features = selection.features();
        let environment = selection.build_env();
        let image_dir = if batch.image == super::catalog::Image::Std {
            "std"
        } else {
            "freestanding"
        };
        let entry: toml::Value = toml::from_str(&std::fs::read_to_string(
            root.join(image_dir).join("Cargo.toml"),
        )?)?;
        let library: toml::Value = toml::from_str(&std::fs::read_to_string(
            root.parent().expect("CPU library").join("Cargo.toml"),
        )?)?;
        let feature_values = |dependency: &str| -> Vec<String> {
            entry
                .get("dependencies")
                .and_then(|deps| deps.get(dependency))
                .and_then(|dep| dep.get("features"))
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(toml::Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        };
        let identity = serde_json::to_vec(&(
            batch.image.package(),
            target,
            &build,
            &features,
            &environment,
            &entry,
            &library,
        ))?;
        runs.push(Run {
            batch: selection.label(),
            package: batch.image.package(),
            mode: batch.mode,
            build_config: build_config.display().to_string(),
            qemu_config: qemu_config.display().to_string(),
            cargo_features: features,
            build_environment: environment,
            entry_capabilities: feature_values("cpu-tests"),
            runtime_features: feature_values("ax-std"),
            build_identity: format!("{:x}", Sha256::digest(identity)),
            build_configuration: build,
            runtime_configuration: runtime,
            tests: selection.tests.clone(),
        });
    }
    let plan = Plan {
        arch,
        target,
        planned_boots: runs.len(),
        logical_tests: runs
            .iter()
            .flat_map(|run| run.tests.iter().map(|test| &test.id))
            .collect::<BTreeSet<_>>()
            .len(),
        test_invocations: runs.iter().map(|run| run.tests.len()).sum(),
        build_identities: runs
            .iter()
            .map(|run| &run.build_identity)
            .collect::<BTreeSet<_>>()
            .len(),
        runs,
    };
    println!("{}", serde_json::to_string_pretty(&plan)?);
    Ok(())
}
