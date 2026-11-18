mod bindings;
mod catalog;
mod listing;

use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, bail};
use bindings::Bindings;
use catalog::{Catalog, Selection};
pub(crate) use listing::{print_plan, render_listing};
use ostool::run::qemu::QemuConfig;

use super::{
    ARCEOS_C_TEST_BUILD_GROUP,
    assets::{arceos_c_test_dir, build_config_path, qemu_config_path},
    types::CTestArtifactPaths,
};
use crate::{
    arceos::{ArceOS, build, cbuild, rootfs},
    context::{BuildCliArgs, SnapshotPersistence},
    execution::{host_http::HostHttpServerGuard, qemu as qemu_test},
};

pub(crate) fn c_qemu_batch_listing(
    root: &Path,
    selected: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    Ok(Catalog::load(root)?
        .select(selected)?
        .into_iter()
        .map(|selection| format!("{} ({} tests; 1 boot)", selection.id, selection.tests.len()))
        .collect())
}
pub(crate) fn arceos_c_test_suit_build_config_path(
    root: &Path,
    target: &str,
) -> anyhow::Result<PathBuf> {
    build_config_path(root, target, "ArceOS C test suite")
}
pub(crate) fn arceos_c_test_suit_qemu_config_path(
    root: &Path,
    arch: &str,
) -> anyhow::Result<PathBuf> {
    qemu_config_path(root, arch, "ArceOS C test suite")
}
fn load_c_test_build_config(path: &Path) -> anyhow::Result<build::ArceosConfig> {
    let config = crate::build::load_config_input::<crate::arceos::config::ArceosConfig>(
        &crate::context::workspace_root_path()?,
        Some(path),
        None,
    )?
    .config;
    if config.app_c.is_none() {
        bail!("ArceOS C build config must set app-c source directory");
    }
    Ok(config)
}
fn load_c_test_qemu_config(path: &Path) -> anyhow::Result<QemuConfig> {
    toml::from_str(&fs::read_to_string(path)?).context("parse C QEMU configuration")
}
fn c_test_artifact_index(arch: &str) -> usize {
    match arch {
        "x86_64" => 0,
        "aarch64" => 1,
        "riscv64" => 2,
        "loongarch64" => 3,
        _ => unreachable!("validated C architecture"),
    }
}
fn c_test_artifact_paths(target: &Path, id: &str, arch: &str) -> CTestArtifactPaths {
    CTestArtifactPaths {
        target_dir: target
            .join("axbuild/arceos-c")
            .join(ARCEOS_C_TEST_BUILD_GROUP)
            .join("cargo"),
        out_dir: target
            .join("axbuild/arceos-c")
            .join(id)
            .join(format!("out-{}", c_test_artifact_index(arch))),
    }
}

pub(crate) async fn test_c_qemu(
    arceos: &mut ArceOS,
    target: &str,
    selected: Option<&str>,
) -> anyhow::Result<()> {
    let arch = crate::context::arch_for_target_checked(target)?;
    let root = arceos_c_test_dir(arceos);
    let catalog = Catalog::load(&root)?;
    let selections = catalog.select(selected)?;
    println!(
        "running arceos C tests: arch={arch} boots={} tests={}",
        selections.len(),
        selections.iter().map(|s| s.tests.len()).sum::<usize>()
    );
    let mut summary = qemu_test::QemuTestSummary::default();
    let started = Instant::now();
    for selection in selections {
        let name = format!("c/{}", selection.id);
        let case_started = Instant::now();
        match build_and_run(arceos, target, arch, &root, &catalog, &selection).await {
            Ok(()) => {
                println!("ok: {name}");
                summary.pass_with_detail(name, format!("{:.2?}", case_started.elapsed()));
            }
            Err(error) => {
                eprintln!("failed: {name}: {error:#}");
                summary.fail_with_detail(name, format!("{error:#}"));
            }
        }
    }
    summary.finish_with_total_detail(
        "arceos c",
        "test",
        Some(&format!("{:.2?}", started.elapsed())),
    )
}

async fn build_and_run(
    arceos: &mut ArceOS,
    target: &str,
    arch: &str,
    root: &Path,
    catalog: &Catalog,
    selection: &Selection<'_>,
) -> anyhow::Result<()> {
    let build_path = arceos_c_test_suit_build_config_path(root, target)?;
    let qemu_path = arceos_c_test_suit_qemu_config_path(root, arch)?;
    let mut qemu = load_c_test_qemu_config(&qemu_path)?;
    let request = arceos.prepare_request(
        BuildCliArgs {
            preset: None,
            config: Some(build_path.clone()),
            package: Some("ax-libc".into()),
            arch: None,
            target: Some(target.into()),
            smp: None,
            debug: false,
        },
        None,
        None,
        SnapshotPersistence::Discard,
    )?;
    // Request resolution makes file-relative paths absolute and applies
    // environment overrides; execution must use that resolved configuration.
    let build::ArceosBuildMode::AppC { app_dir, .. } = build::arceos_build_mode(&request.config)?
    else {
        bail!("C suite requires app-c");
    };
    let artifacts = c_test_artifact_paths(arceos.app.target_dir(), selection.id, arch);
    let bindings = Bindings::new(catalog, selection);
    let generated = arceos.app.target_dir().join("axbuild/arceos-c/generated");
    let (include, header) = bindings.write(&generated)?;
    let mut features = request.config.common.features.clone();
    features.extend(selection.features());
    features.sort();
    features.dedup();
    let main = app_dir.join("main.c");
    let input = cbuild::ArceosCBuildInput {
        app_dir,
        app_name: ARCEOS_C_TEST_BUILD_GROUP.into(),
        target_dir: artifacts.target_dir,
        out_dir: artifacts.out_dir.join(bindings.identity()),
        features,
        include_dirs: vec![include],
        source_flags: std::collections::BTreeMap::from([(
            main,
            vec![format!(
                "-DARCEOS_C_SELECTED_HEADER={}",
                bindings::literal(&header.to_string_lossy())
            )],
        )]),
    };
    let output = cbuild::build_c_app(arceos.app.workspace_context(), &request, &input)?;
    let map = output.elf_path.with_extension("axbt");
    crate::build::symbol_map::generate_axbt_map(
        &output.elf_path,
        &output.elf_path.with_extension("axbt"),
    )?;
    qemu_test::validate_test_qemu_rootfs_write_policy(&qemu_path, "ArceOS")?;
    rootfs::prepare_default_qemu_fat32_rootfs(arceos.app.workspace_root(), &qemu)?;
    rootfs::isolate_qemu_test_rootfs(&mut qemu)?;
    let symbol_initramfs = qemu_test::prepare_qemu_initramfs_with_symbols(&qemu, &map, target)?;
    if let Some(archive) = &symbol_initramfs {
        qemu.boot.initramfs = Some(archive.path().to_string_lossy().into_owned());
    }
    crate::support::qemu_success::replace_configured_success_regex(
        &mut qemu,
        vec![format!(
            r"(?m)^{}\s*$",
            regex::escape(&selection.completion())
        )],
    );
    let _server = qemu_test::load_qemu_case_host_http_server(&qemu_path)?
        .as_ref()
        .map(|config| HostHttpServerGuard::start(config, selection.id))
        .transpose()?;
    arceos
        .app
        .prepare_elf_artifact(output.elf_path, qemu.to_bin)
        .await?;
    arceos.app.run_prepared_qemu(qemu).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn c_runner_rejects_a_rust_only_build_configuration() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("build.toml");
        fs::write(&path, "features = []\nlog = \"Info\"\n").unwrap();
        assert!(
            load_c_test_build_config(&path)
                .unwrap_err()
                .to_string()
                .contains("must set app-c")
        );
    }
}
