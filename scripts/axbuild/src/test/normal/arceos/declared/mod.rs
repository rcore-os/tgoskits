//! Explicit, repository-neutral batch execution over the shared ArceOS runner.
pub(crate) mod catalog;
mod listing;

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use catalog::{Catalog, Image, Mode, Selection};
pub(crate) use listing::{listed_cases, print_plan, render_listing};

use super::{
    assets::arceos_test_group_dir,
    qemu::{prepare_resolved_qemu_cases, resolve_qemu_case},
    runner::run_prepared_qemu_groups,
    types::{ArceosQemuCase, ResolvedArceosQemuCase},
};
use crate::{arceos::ArceOS, execution::case::TestQemuCase};

/// Use Cargo's existing metadata namespace to opt a group into ordered batches.
pub(crate) fn manifest_path(workspace: &Path, group: &str) -> anyhow::Result<Option<PathBuf>> {
    let root = arceos_test_group_dir(workspace, group);
    let package = root.join("Cargo.toml");
    if !package.is_file() {
        return Ok(None);
    }
    let manifest: toml::Value = toml::from_str(&fs::read_to_string(&package)?)
        .with_context(|| format!("invalid {}", package.display()))?;
    let Some(source) = manifest
        .get("package")
        .and_then(|v| v.get("metadata"))
        .and_then(|v| v.get("axbuild"))
        .and_then(|v| v.get("qemu_suite"))
    else {
        return Ok(None);
    };
    let path = Path::new(
        source
            .as_str()
            .context("package.metadata.axbuild.qemu_suite must be a path string")?,
    );
    ensure!(
        catalog::relative_path(path),
        "execution manifest must stay inside its group"
    );
    let declaration = root.join(path).canonicalize()?;
    ensure!(
        declaration.starts_with(root.canonicalize()?),
        "execution manifest escapes group"
    );
    Ok(Some(declaration))
}

fn resolve_cases(
    arceos: &ArceOS,
    root: &Path,
    target: &str,
    selections: &[Selection<'_>],
) -> anyhow::Result<Vec<ResolvedArceosQemuCase>> {
    selections
        .iter()
        .map(|selection| {
            let batch = selection.batch;
            let case = ArceosQemuCase {
                case: TestQemuCase {
                    name: selection.label(),
                    display_name: selection.label(),
                    case_dir: root.to_path_buf(),
                    qemu_config_path: root.join(&batch.qemu_config),
                    test_commands: Vec::new(),
                    grouped_command_selection: Default::default(),
                    host_http_server: None,
                    subcases: Vec::new(),
                    grouped_subcase_filter: None,
                    ltp_case_id: None,
                },
                build_group: selection.label(),
                build_config_path: root.join(&batch.build_config),
                package: batch.package.clone(),
                feature: Some(selection.id.to_owned()),
                features: selection.features(),
            };
            let mut resolved = resolve_qemu_case(arceos, target, case)?;
            ensure!(
                resolved.request.arch == batch.arch
                    && resolved.request.config.freestanding == (batch.image == Image::Freestanding),
                "declared architecture or runtime differs from resolved build configuration"
            );
            resolved.cargo.env.extend(selection.build_env());
            Ok(resolved)
        })
        .collect()
}

fn runtime_configuration(root: &Path, selection: &Selection<'_>) -> anyhow::Result<toml::Value> {
    let runtime: toml::Value = toml::from_str(&fs::read_to_string(
        root.join(&selection.batch.qemu_config),
    )?)
    .context("declared runtime configuration")?;
    ensure!(
        !selection.verifies_pcm() || runtime.get("host_pcm_waveform").is_some(),
        "selected member requires host PCM verification"
    );
    Ok(runtime)
}

fn completion_regex(selection: &Selection<'_>) -> String {
    format!(r"(?m)^{}\s*$", regex::escape(&selection.completion()))
}

pub(crate) async fn run(
    arceos: &mut ArceOS,
    manifest: &Path,
    group: &str,
    arch: &str,
    target: &str,
    selected: Option<&str>,
    allow_missing: bool,
) -> anyhow::Result<()> {
    let catalog = Catalog::load(manifest)?;
    let selections = match catalog.select(arch, selected) {
        Ok(selections) => selections,
        Err(_)
            if allow_missing
                && selected.is_none()
                && !catalog.batches.values().any(|b| b.arch == arch) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let root = manifest.parent().context("execution suite directory")?;
    for selection in &selections {
        runtime_configuration(root, selection)?;
    }
    let resolved = resolve_cases(arceos, root, target, &selections)?;
    let mut prepared = prepare_resolved_qemu_cases(arceos, resolved, None).await?;
    for (case, selection) in prepared.iter_mut().zip(&selections) {
        case.verify_pcm_output = selection.verifies_pcm();
        if selection.batch.mode == Mode::Normal {
            crate::support::qemu_success::replace_configured_success_regex(
                &mut case.qemu,
                vec![completion_regex(selection)],
            );
        }
    }
    println!(
        "running arceos {group}: arch={arch} boots={} tests={}",
        prepared.len(),
        selections.iter().map(|s| s.tests.len()).sum::<usize>()
    );
    run_prepared_qemu_groups(arceos, group, &format!("arceos {group}"), &prepared).await
}
