//! Shared ArceOS Rust-program startup used by suite and independent projects.
use anyhow::Context;
use ostool::{build::config::Cargo, run::qemu::QemuConfig};

use super::{
    assets::test_build_args,
    types::{ArceosQemuCase, PreparedArceosQemuCase},
};
use crate::{
    arceos::{ArceOS, build, rootfs},
    context::SnapshotPersistence,
    execution::{host_http::HostHttpServerGuard, qemu as qemu_test},
};

pub(crate) async fn prepare_qemu_cases(
    arceos: &mut ArceOS,
    target: &str,
    cases: Vec<ArceosQemuCase>,
    configure: Option<fn(&mut QemuConfig, Option<&str>)>,
) -> anyhow::Result<Vec<PreparedArceosQemuCase>> {
    let mut prepared = Vec::with_capacity(cases.len());
    for case in cases {
        qemu_test::validate_test_qemu_rootfs_write_policy(&case.case.qemu_config_path, "ArceOS")?;
        let request = arceos.prepare_request(
            test_build_args(&case.package, target, &case.build_config_path),
            Some(case.case.qemu_config_path.clone()),
            None,
            SnapshotPersistence::Discard,
        )?;
        let mut cargo = build::load_cargo_config(&request, arceos.app.workspace_context())?;
        if case.feature.is_some() {
            for feature in &case.features {
                add_cargo_feature(&mut cargo, feature);
            }
            let case_enables_mmu = crate::build::package_enables_mmu(
                &request.package,
                arceos.app.workspace_context().metadata(),
                &case.features,
            )?;
            if case_enables_mmu {
                crate::build::append_cargo_rustflags(&mut cargo, &["-Zstack-protector=strong"]);
            }
        }
        let mut qemu = arceos
            .load_qemu_config(&request, &cargo)
            .await?
            .with_context(|| {
                format!(
                    "failed to load ArceOS qemu config for case `{}`",
                    case.case.display_name
                )
            })?;
        qemu_test::apply_smp_qemu_arg(
            &mut qemu,
            request
                .smp
                .or(request.config.common.max_cpu_num)
                .or(Some(1)),
        );
        if let Some(configure) = configure {
            configure(&mut qemu, case.feature.as_deref());
        }
        qemu_test::apply_timeout_scale(&mut qemu);
        rootfs::prepare_default_qemu_fat32_rootfs(arceos.app.workspace_root(), &qemu)?;
        rootfs::isolate_qemu_test_rootfs(&mut qemu)?;
        prepared.push(PreparedArceosQemuCase {
            case,
            request,
            cargo,
            qemu,
        });
    }
    Ok(prepared)
}

fn add_cargo_feature(cargo: &mut Cargo, feature: &str) {
    // The feature comes from the explicitly selected ArceOS test case; the
    // normal suite path never adds test-specific capabilities.
    if !cargo.features.iter().any(|existing| existing == feature) {
        cargo.features.push(feature.to_string());
        cargo.features.sort();
    }
}

pub(crate) async fn run_qemu_case(
    arceos: &mut ArceOS,
    case: &PreparedArceosQemuCase,
    map_path: &std::path::Path,
) -> anyhow::Result<()> {
    let case_name = &case.case.case.name;

    let _host_http_server = case
        .case
        .case
        .host_http_server
        .as_ref()
        .map(|config| HostHttpServerGuard::start(config, case_name))
        .transpose()?;

    let mut qemu = case.qemu.clone();
    let pcm_waveform = super::pcm_waveform::PcmWaveform::start(
        &case.case.case.qemu_config_path,
        arceos.app.target_dir(),
        &mut qemu,
    )?;
    qemu_test::append_backtrace_map(&mut qemu, map_path, &case.request.target)?;
    let serial_rx = if case.case.feature.as_deref() == Some("serial-rx") {
        Some(super::serial_rx::SerialRxFixture::start(&mut qemu).await?)
    } else {
        None
    };
    let result = arceos
        .app
        .run_qemu_with_axtest_coverage(&case.cargo, qemu)
        .await
        .with_context(|| format!("failed to run ArceOS rust qemu test case `{case_name}`"));
    if let Some(fixture) = serial_rx {
        fixture.finish().await?;
    }
    result?;
    if let Some(recording) = pcm_waveform {
        recording.verify()?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::{arceos::test::ARCEOS_RUST_TEST_PACKAGE, execution::case::TestQemuCase};

    fn rust_test_suite_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-suit/arceos/rust")
    }

    #[tokio::test]
    async fn arceos_rust_case_preparation_rejects_persistent_rootfs_policy() {
        let root = tempfile::tempdir().unwrap();
        let qemu_config_path = write_test_qemu_config(root.path(), Some("persist"));
        let mut arceos = ArceOS::new().unwrap();

        let error = prepare_qemu_cases(
            &mut arceos,
            "riscv64gc-unknown-none-elf",
            vec![rust_qemu_case(qemu_config_path)],
            None,
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("cannot use `rootfs_write_policy = \"persist\"`"));
    }

    #[tokio::test]
    async fn arceos_rust_case_preparation_isolates_its_rootfs_writes() {
        let root = tempfile::tempdir().unwrap();
        let qemu_config_path = write_test_qemu_config(root.path(), None);
        let mut arceos = ArceOS::new().unwrap();

        let prepared = prepare_qemu_cases(
            &mut arceos,
            "riscv64gc-unknown-none-elf",
            vec![rust_qemu_case(qemu_config_path)],
            None,
        )
        .await
        .unwrap();

        assert!(
            prepared[0].qemu.args.iter().any(|argument| {
                argument.contains("id=disk0") && argument.contains("snapshot=on")
            })
        );
    }

    fn rust_qemu_case(qemu_config_path: PathBuf) -> ArceosQemuCase {
        ArceosQemuCase {
            case: TestQemuCase {
                name: "task-ipi".to_string(),
                display_name: "task-ipi".to_string(),
                case_dir: qemu_config_path.parent().unwrap().to_path_buf(),
                qemu_config_path,
                test_commands: Vec::new(),
                grouped_command_selection: Default::default(),
                host_http_server: None,
                subcases: Vec::new(),
                grouped_subcase_filter: None,
                ltp_case_id: None,
            },
            build_group: "arceos-rust".to_string(),
            build_config_path: rust_test_suite_root().join("build-riscv64gc-unknown-none-elf.toml"),
            package: ARCEOS_RUST_TEST_PACKAGE.to_string(),
            feature: Some("task-ipi".to_string()),
            features: vec!["task-ipi".to_string()],
        }
    }

    fn write_test_qemu_config(root: &Path, rootfs_write_policy: Option<&str>) -> PathBuf {
        let disk_path = root.join("disk.img");
        std::fs::write(&disk_path, []).unwrap();
        let policy = rootfs_write_policy
            .map(|policy| format!("rootfs_write_policy = \"{policy}\"\n"))
            .unwrap_or_default();
        let qemu_config_path = root.join("qemu-riscv64.toml");
        std::fs::write(
            &qemu_config_path,
            format!(
                r#"args = [
    "-m",
    "64M",
    "-smp",
    "4",
    "-drive",
    "id=disk0,if=none,format=raw,file={}",
]

timeout = 5
uefi = false
to_bin = false
success_regex = ["OK"]
fail_regex = ["FAIL"]
{policy}"#,
                disk_path.display()
            ),
        )
        .unwrap();
        qemu_config_path
    }
}
