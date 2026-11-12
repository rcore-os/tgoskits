use std::{collections::BTreeSet, path::Path};

use anyhow::Context;

use super::{
    catalog::{Catalog, Mode},
    completion_regex, resolve_cases, runtime_configuration,
};
use crate::{arceos::ArceOS, execution::qemu::ListedQemuCase};

pub(crate) fn listed_cases(
    manifest: &Path,
    requested: Option<&str>,
) -> anyhow::Result<Vec<ListedQemuCase>> {
    let catalog = Catalog::load(manifest)?;
    let arches: BTreeSet<_> = catalog
        .batches
        .values()
        .map(|batch| batch.arch.as_str())
        .collect();
    let mut cases = Vec::new();
    for arch in arches {
        let selections = match catalog.select(arch, requested) {
            Ok(selections) => selections,
            Err(_) if requested.is_some() => continue,
            Err(error) => return Err(error),
        };
        cases.extend(selections.into_iter().map(|s| ListedQemuCase {
            name: s.label(),
            archs: vec![arch.to_owned()],
        }));
    }
    Ok(cases)
}

pub(crate) fn render_listing(
    manifest: &Path,
    group: &str,
    arch: Option<&str>,
    requested: Option<&str>,
) -> anyhow::Result<String> {
    let catalog = Catalog::load(manifest)?;
    let mut lines = vec![group.to_owned()];
    let mut boots = 0;
    for current in crate::context::supported_arches() {
        if arch.is_some_and(|a| a != current)
            || !catalog.batches.values().any(|b| b.arch == *current)
        {
            continue;
        }
        let selections = match catalog.select(current, requested) {
            Ok(selections) => selections,
            Err(_) if arch.is_none() && requested.is_some() => continue,
            Err(error) => return Err(error),
        };
        for selection in selections {
            boots += 1;
            lines.push(format!(
                "  {boots}. {} [{current} {:?} {:?}] tests={} boots=1",
                selection.label(),
                selection.batch.image,
                selection.batch.mode,
                selection.tests.len()
            ));
            lines.extend(
                selection
                    .tests
                    .iter()
                    .map(|t| format!("     {}: {} -> {}", t.id, t.name, t.entry)),
            );
        }
    }
    anyhow::ensure!(boots > 0, "no declared tests match {requested:?}");
    lines.push(format!("total boots: {boots}"));
    Ok(lines.join("\n"))
}

pub(crate) fn print_plan(
    arceos: &ArceOS,
    manifest: &Path,
    arch: &str,
    target: &str,
    requested: Option<&str>,
) -> anyhow::Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&plan_value(arceos, manifest, arch, target, requested)?)?
    );
    Ok(())
}

fn plan_value(
    arceos: &ArceOS,
    manifest: &Path,
    arch: &str,
    target: &str,
    requested: Option<&str>,
) -> anyhow::Result<serde_json::Value> {
    let catalog = Catalog::load(manifest)?;
    let selections = catalog.select(arch, requested)?;
    let root = manifest.parent().context("execution suite directory")?;
    let resolved = resolve_cases(arceos, root, target, &selections)?;
    let mut rows = Vec::new();
    for (case, selection) in resolved.iter().zip(&selections) {
        let runtime = runtime_configuration(root, selection)?;
        let identity =
            crate::support::fingerprint::signature(&(&case.cargo, &case.request.config))?;
        let effective = if selection.batch.mode == Mode::Normal {
            vec![completion_regex(selection)]
        } else {
            Vec::new()
        };
        rows.push(serde_json::json!({
            "batch":selection.label(),"package":case.request.package,"mode":selection.batch.mode,
            "cargo_features":case.cargo.features,"build_environment":case.cargo.env,
            "cargo_configuration":case.cargo,"build_identity":identity,
            "build_config":case.case.build_config_path,"qemu_config":case.case.case.qemu_config_path,
            "build_configuration":case.request.config,"runtime_configuration":runtime,
            "members":selection.members(),"tests":selection.tests,
            "completion":selection.completion(),"effective_success_regex":effective,
            "verify_pcm_waveform":selection.verifies_pcm()
        }));
    }
    Ok(serde_json::json!({
        "arch":arch,"target":target,"planned_boots":rows.len(),
        "logical_tests":selections.iter().flat_map(|s|s.tests.iter().map(|t|&t.id)).collect::<BTreeSet<_>>().len(),
        "test_invocations":selections.iter().map(|s|s.tests.len()).sum::<usize>(),
        "build_identities":rows.iter().map(|r|r["build_identity"].as_str()).collect::<BTreeSet<_>>().len(),
        "runs":rows
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    const CHILD: &str = "AXBUILD_TEST_DECLARED_PLAN";
    const ROOT: &str = "AXBUILD_TEST_DECLARED_ROOT";

    #[test]
    fn plan_tracks_effective_build_configuration_without_preparing_assets() {
        if let Ok(scenario) = std::env::var(CHILD) {
            let root = std::path::PathBuf::from(std::env::var_os(ROOT).unwrap());
            let arceos = ArceOS {
                app: crate::context::AppContext::from_workspace_root(
                    &root,
                    Some(&root.join("artifacts")),
                )
                .unwrap(),
            };
            let plan = plan_value(
                &arceos,
                &root.join("suite.toml"),
                "riscv64",
                "riscv64gc-unknown-none-elf",
                None,
            )
            .unwrap();
            assert!(
                plan_value(
                    &arceos,
                    &root.join("suite.toml"),
                    "riscv64",
                    "riscv64gc-unknown-none-elf",
                    Some("missing")
                )
                .is_err()
            );
            assert!(!root.join("artifacts").exists());
            assert!(!root.join(".arceos.toml").exists());
            std::fs::write(
                root.join(format!("{scenario}.json")),
                serde_json::to_vec(&plan).unwrap(),
            )
            .unwrap();
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers=[\"entry\",\"runtime\"]\nresolver=\"3\"\n",
        )
        .unwrap();
        for (directory, name) in [("entry", "example-image"), ("runtime", "ax-std")] {
            std::fs::create_dir_all(root.join(directory).join("src")).unwrap();
            std::fs::write(root.join(directory).join("src/lib.rs"), "").unwrap();
            let dependency = if directory == "entry" {
                "[dependencies]\nax-std={path=\"../runtime\"}\n"
            } else {
                ""
            };
            std::fs::write(
                root.join(directory).join("Cargo.toml"),
                format!(
                    r#"[package]
name={name:?}
version="0.0.0"
edition="2024"
[features]
default=[]
smp=[]
paging=[]
extra=[]
{dependency}"#
                ),
            )
            .unwrap();
        }
        std::fs::write(
            root.join("build.toml"),
            "freestanding=false\nfeatures=[]\nmax_cpu_num=1\n",
        )
        .unwrap();
        std::fs::write(root.join("run.toml"), "args=[]\n").unwrap();
        std::fs::write(
            root.join("suite.toml"),
            r#"
schema_version=1
run_order=["operation"]
[execution]
batch_env="DEMO_BATCH"
members_env="DEMO_MEMBERS"
completion="DEMO_OK batch={batch} selection={selection}"
[batches.operation]
arch="riscv64"
package="example-image"
image="std"
mode="normal"
build_config="build.toml"
qemu_config="run.toml"
[[batches.operation.tests]]
id="transfer"
name="message transfer"
entry="example::transfer"
"#,
        )
        .unwrap();
        let mut plans = Vec::new();
        for (scenario, feature) in [
            ("plain", ""),
            ("overridden", "extra"),
            ("repeated", "extra"),
        ] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child.env_clear();
            for name in [
                "PATH",
                "HOME",
                "RUSTUP_HOME",
                "RUSTUP_TOOLCHAIN",
                "CARGO_HOME",
                "LD_LIBRARY_PATH",
                "SYSTEMROOT",
            ] {
                if let Some(value) = std::env::var_os(name) {
                    child.env(name, value);
                }
            }
            let result=child.arg("--exact")
                .arg("test::normal::arceos::declared::listing::tests::plan_tracks_effective_build_configuration_without_preparing_assets")
                .env(CHILD,scenario).env(ROOT,root).env("FEATURES",feature).output().unwrap();
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            plans.push(
                serde_json::from_slice::<serde_json::Value>(
                    &std::fs::read(root.join(format!("{scenario}.json"))).unwrap(),
                )
                .unwrap(),
            );
        }
        let row = |i: usize| &plans[i]["runs"][0];
        assert!(
            !row(0)["cargo_features"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("extra"))
        );
        assert!(
            row(1)["cargo_features"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("extra"))
        );
        assert_ne!(row(0)["build_identity"], row(1)["build_identity"]);
        assert_eq!(row(1)["build_identity"], row(2)["build_identity"]);
        assert_eq!(
            row(1)["cargo_features"],
            row(1)["cargo_configuration"]["features"]
        );
    }
}
