use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use ostool::{build::CargoQemuOverrideArgs, run::qemu::QemuConfig};
use tokio::fs as tokio_fs;

use super::{board, rootfs};
use crate::context::{
    QemuRunConfig, ResolvedStarryRequest, arch_for_target_checked, starry_target_for_arch_checked,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StarryTestGroup {
    Normal,
    Stress,
}

impl StarryTestGroup {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Stress => "stress",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryQemuCase {
    pub(crate) name: String,
    pub(crate) qemu_config_path: PathBuf,
}

pub(crate) fn parse_test_target(
    workspace_root: &Path,
    target: &str,
) -> anyhow::Result<(String, String)> {
    let supported_targets = board::board_default_list(workspace_root)?
        .into_iter()
        .filter(|board| board.name.starts_with("qemu-"))
        .map(|board| board.target)
        .collect::<Vec<_>>();

    let supported_target_refs = supported_targets
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let supported_arches = supported_targets
        .iter()
        .map(|target| arch_for_target_checked(target))
        .collect::<anyhow::Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();

    if target.contains('-') {
        validate_supported_target(
            target,
            "starry qemu tests",
            "targets",
            &supported_target_refs,
        )?;
        Ok((
            arch_for_target_checked(target)?.to_string(),
            target.to_string(),
        ))
    } else {
        validate_supported_target(
            target,
            "starry qemu tests",
            "arch values",
            &supported_arches,
        )?;
        Ok((
            target.to_string(),
            starry_target_for_arch_checked(target)?.to_string(),
        ))
    }
}

pub(crate) fn discover_qemu_cases(
    workspace_root: &Path,
    arch: &str,
    selected_case: Option<&str>,
    group: StarryTestGroup,
) -> anyhow::Result<Vec<StarryQemuCase>> {
    let test_suite_dir = test_suite_dir(workspace_root, group);
    let config_name = qemu_config_name(arch);

    if let Some(case_name) = selected_case {
        let case_dir = test_suite_dir.join(case_name);
        if !case_dir.is_dir() {
            bail!(
                "unknown Starry {} test case `{case_name}` in {}; available cases are discovered \
                 from direct subdirectories",
                group.as_str(),
                test_suite_dir.display()
            );
        }

        let qemu_config_path = case_dir.join(&config_name);
        if !qemu_config_path.is_file() {
            bail!(
                "Starry test case `{case_name}` does not provide `{}`",
                qemu_config_path.display()
            );
        }

        return Ok(vec![StarryQemuCase {
            name: case_name.to_string(),
            qemu_config_path,
        }]);
    }

    let mut cases = fs::read_dir(&test_suite_dir)
        .with_context(|| format!("failed to read {}", test_suite_dir.display()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }

            let name = entry.file_name().into_string().ok()?;
            let qemu_config_path = path.join(&config_name);
            if qemu_config_path.is_file() {
                Some(StarryQemuCase {
                    name,
                    qemu_config_path,
                })
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    cases.sort_by(|left, right| left.name.cmp(&right.name));

    if cases.is_empty() {
        bail!(
            "no Starry {} qemu test cases for arch `{arch}` found under {}",
            group.as_str(),
            test_suite_dir.display()
        );
    }

    Ok(cases)
}

pub(crate) async fn prepare_test_qemu_run_config(
    workspace_root: &Path,
    request: &ResolvedStarryRequest,
    template_path: &Path,
    timeout_override: Option<u64>,
) -> anyhow::Result<QemuRunConfig> {
    let base_disk_img =
        rootfs::ensure_rootfs_in_target_dir(workspace_root, &request.arch, &request.target).await?;
    let isolated_disk_img = isolated_test_disk_image_path(workspace_root, request)?;
    tokio_fs::copy(&base_disk_img, &isolated_disk_img)
        .await
        .with_context(|| {
            format!(
                "failed to copy {} to {}",
                base_disk_img.display(),
                isolated_disk_img.display()
            )
        })?;

    let config = tokio_fs::read_to_string(template_path)
        .await
        .with_context(|| format!("failed to read {}", template_path.display()))?;
    let mut config: QemuConfig = toml::from_str(&config)
        .with_context(|| format!("failed to parse {}", template_path.display()))?;

    replace_workspace_variables(&mut config, workspace_root);

    let shared_disk = expand_workspace_variables(
        &shared_rootfs_image_path(&request.target, &request.arch)?,
        workspace_root,
    );
    replace_rootfs_arg(
        &mut config.args,
        &shared_disk,
        &isolated_disk_img.display().to_string(),
    );
    apply_timeout_override(&mut config, timeout_override);

    Ok(QemuRunConfig {
        qemu_config: Some(runtime_qemu_template_path(workspace_root)),
        timeout_seconds: config.timeout.filter(|seconds| *seconds > 0),
        override_args: qemu_override_args(config),
        ..Default::default()
    })
}

pub(crate) fn finalize_qemu_case_run(
    failed: &[String],
    group: StarryTestGroup,
) -> anyhow::Result<()> {
    if failed.is_empty() {
        println!("all starry {} qemu test cases passed", group.as_str());
        Ok(())
    } else {
        bail!(
            "starry {} qemu tests failed for {} case(s): {}",
            group.as_str(),
            failed.len(),
            failed.join(", ")
        )
    }
}

fn validate_supported_target(
    target: &str,
    suite_name: &str,
    supported_kind: &str,
    supported: &[&str],
) -> anyhow::Result<()> {
    if supported.contains(&target) {
        Ok(())
    } else {
        bail!(
            "unsupported target `{}` for {}. Supported {} are: {}",
            target,
            suite_name,
            supported_kind,
            supported.join(", ")
        )
    }
}

fn test_suite_dir(workspace_root: &Path, group: StarryTestGroup) -> PathBuf {
    workspace_root
        .join("test-suit")
        .join("starryos")
        .join(group.as_str())
}

fn qemu_config_name(arch: &str) -> String {
    format!("qemu-{arch}.toml")
}

fn shared_rootfs_image_path(target: &str, arch: &str) -> anyhow::Result<String> {
    Ok(format!(
        "${{workspace}}/target/{target}/{}",
        rootfs::rootfs_image_name(arch)?
    ))
}

fn isolated_test_disk_image_path(
    workspace_root: &Path,
    request: &ResolvedStarryRequest,
) -> anyhow::Result<PathBuf> {
    let target_dir = rootfs::resolve_target_dir(workspace_root, &request.target)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time is before unix epoch")?
        .as_nanos();
    Ok(target_dir.join(format!("disk-test-{}-{timestamp}.img", std::process::id())))
}

fn runtime_qemu_template_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("os/StarryOS/starryos/.qemu-aarch64.toml")
}

fn expand_workspace_variables(input: &str, workspace_root: &Path) -> String {
    let workspace = workspace_root.display().to_string();
    input
        .replace("${workspace}", &workspace)
        .replace("${workspaceFolder}", &workspace)
}

fn replace_workspace_variables(config: &mut QemuConfig, workspace_root: &Path) {
    config.args = config
        .args
        .iter()
        .map(|arg| expand_workspace_variables(arg, workspace_root))
        .collect();
    config.success_regex = config
        .success_regex
        .iter()
        .map(|pattern| expand_workspace_variables(pattern, workspace_root))
        .collect();
    config.fail_regex = config
        .fail_regex
        .iter()
        .map(|pattern| expand_workspace_variables(pattern, workspace_root))
        .collect();
    config.shell_prefix = config
        .shell_prefix
        .as_deref()
        .map(|value| expand_workspace_variables(value, workspace_root));
    config.shell_init_cmd = config
        .shell_init_cmd
        .as_deref()
        .map(|value| expand_workspace_variables(value, workspace_root));
}

fn replace_rootfs_arg(args: &mut Vec<String>, shared_rootfs: &str, isolated_rootfs: &str) {
    for arg in args {
        if arg.contains(shared_rootfs) {
            *arg = arg.replace(shared_rootfs, isolated_rootfs);
        }
    }
}

fn apply_timeout_override(config: &mut QemuConfig, timeout_override: Option<u64>) {
    match timeout_override {
        None => {}
        Some(0) => config.timeout = None,
        Some(seconds) => config.timeout = Some(seconds),
    }
}

fn qemu_override_args(config: QemuConfig) -> CargoQemuOverrideArgs {
    CargoQemuOverrideArgs {
        to_bin: Some(config.to_bin),
        args: Some(config.args),
        success_regex: Some(config.success_regex),
        fail_regex: Some(config.fail_regex),
        shell_prefix: config.shell_prefix,
        shell_init_cmd: config.shell_init_cmd,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::test_qemu;

    fn write_starry_workspace(root: &Path) {
        let starry_workspace_dir = root.join("os/StarryOS");
        let starry_dir = root.join("os/StarryOS/starryos");
        let src_dir = starry_dir.join("src");
        fs::create_dir_all(root.join("os/StarryOS/configs/board")).unwrap();
        fs::create_dir_all(&starry_workspace_dir).unwrap();
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), "").unwrap();
        fs::write(
            starry_dir.join("Cargo.toml"),
            "[package]\nname = \"starryos\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            starry_workspace_dir.join("Cargo.toml"),
            "[workspace]\nmembers = [\"starryos\"]\n",
        )
        .unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"os/StarryOS/starryos\"]\n",
        )
        .unwrap();
    }

    fn write_starry_board(root: &Path, name: &str, target: &str) {
        fs::write(
            root.join("os/StarryOS/configs/board")
                .join(format!("{name}.toml")),
            format!(
                "target = \"{target}\"\nenv = {{ AX_IP = \"10.0.2.15\", AX_GW = \"10.0.2.2\" \
                 }}\nlog = \"Warn\"\nfeatures = [\"qemu\"]\nplat_dyn = false\n"
            ),
        )
        .unwrap();
    }

    fn write_case(root: &Path, group: StarryTestGroup, case_name: &str, arch: &str, body: &str) {
        let case_dir = test_suite_dir(root, group).join(case_name);
        fs::create_dir_all(&case_dir).unwrap();
        fs::write(case_dir.join(qemu_config_name(arch)), body).unwrap();
    }

    fn write_runtime_qemu_template(root: &Path) {
        let starry_dir = root.join("os/StarryOS/starryos");
        fs::create_dir_all(&starry_dir).unwrap();
        fs::write(
            starry_dir.join(".qemu-aarch64.toml"),
            "args = []\nuefi = false\nto_bin = true\nsuccess_regex = []\nfail_regex = []\n",
        )
        .unwrap();
    }

    #[test]
    fn parses_supported_starry_arch_aliases() {
        let root = tempdir().unwrap();
        write_starry_workspace(root.path());
        write_starry_board(root.path(), "qemu-x86_64", "x86_64-unknown-none");
        write_starry_board(
            root.path(),
            "qemu-aarch64",
            "aarch64-unknown-none-softfloat",
        );

        assert_eq!(
            parse_test_target(root.path(), "x86_64").unwrap(),
            ("x86_64".to_string(), "x86_64-unknown-none".to_string())
        );
        assert_eq!(
            parse_test_target(root.path(), "aarch64").unwrap(),
            (
                "aarch64".to_string(),
                "aarch64-unknown-none-softfloat".to_string()
            )
        );
    }

    #[test]
    fn accepts_starry_full_target_triples() {
        let root = tempdir().unwrap();
        write_starry_workspace(root.path());
        write_starry_board(root.path(), "qemu-x86_64", "x86_64-unknown-none");

        assert_eq!(
            parse_test_target(root.path(), "x86_64-unknown-none").unwrap(),
            ("x86_64".to_string(), "x86_64-unknown-none".to_string())
        );
    }

    #[test]
    fn discovers_normal_cases_in_lexicographic_order() {
        let root = tempdir().unwrap();
        write_case(
            root.path(),
            StarryTestGroup::Normal,
            "apk",
            "riscv64",
            "timeout = 10\n",
        );
        write_case(
            root.path(),
            StarryTestGroup::Normal,
            "smoke",
            "riscv64",
            "timeout = 5\n",
        );
        write_case(
            root.path(),
            StarryTestGroup::Stress,
            "stress-ng-0",
            "riscv64",
            "timeout = 5\n",
        );

        let cases =
            discover_qemu_cases(root.path(), "riscv64", None, StarryTestGroup::Normal).unwrap();
        assert_eq!(
            cases
                .iter()
                .map(|case| case.name.as_str())
                .collect::<Vec<_>>(),
            vec!["apk", "smoke"]
        );
    }

    #[test]
    fn discovers_stress_cases() {
        let root = tempdir().unwrap();
        write_case(
            root.path(),
            StarryTestGroup::Normal,
            "smoke",
            "riscv64",
            "timeout = 5\n",
        );
        write_case(
            root.path(),
            StarryTestGroup::Stress,
            "stress-ng-0",
            "riscv64",
            "timeout = 10\n",
        );

        let cases =
            discover_qemu_cases(root.path(), "riscv64", None, StarryTestGroup::Stress).unwrap();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].name, "stress-ng-0");
    }

    #[test]
    fn filters_to_selected_case_in_current_group() {
        let root = tempdir().unwrap();
        write_case(
            root.path(),
            StarryTestGroup::Normal,
            "smoke",
            "riscv64",
            "timeout = 5\n",
        );
        write_case(
            root.path(),
            StarryTestGroup::Stress,
            "stress-ng-0",
            "riscv64",
            "timeout = 10\n",
        );

        let cases = discover_qemu_cases(
            root.path(),
            "riscv64",
            Some("stress-ng-0"),
            StarryTestGroup::Stress,
        )
        .unwrap();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].name, "stress-ng-0");
    }

    #[test]
    fn rejects_missing_selected_case() {
        let root = tempdir().unwrap();
        write_case(
            root.path(),
            StarryTestGroup::Normal,
            "smoke",
            "riscv64",
            "timeout = 5\n",
        );

        let err = discover_qemu_cases(
            root.path(),
            "riscv64",
            Some("missing"),
            StarryTestGroup::Normal,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("unknown Starry normal test case `missing`")
        );
    }

    #[test]
    fn rejects_selected_case_without_target_arch() {
        let root = tempdir().unwrap();
        write_case(
            root.path(),
            StarryTestGroup::Normal,
            "smoke",
            "aarch64",
            "timeout = 5\n",
        );

        let err = discover_qemu_cases(
            root.path(),
            "riscv64",
            Some("smoke"),
            StarryTestGroup::Normal,
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not provide"));
        assert!(err.to_string().contains("qemu-riscv64.toml"));
    }

    #[test]
    fn rejects_selected_case_from_other_group() {
        let root = tempdir().unwrap();
        write_case(
            root.path(),
            StarryTestGroup::Stress,
            "stress-ng-0",
            "riscv64",
            "timeout = 5\n",
        );

        let err = discover_qemu_cases(
            root.path(),
            "riscv64",
            Some("stress-ng-0"),
            StarryTestGroup::Normal,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("unknown Starry normal test case `stress-ng-0`")
        );
    }

    #[tokio::test]
    async fn prepare_test_qemu_run_config_rewrites_shared_disk_path() {
        let root = tempdir().unwrap();
        let target_dir = root.path().join("target/x86_64-unknown-none");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(target_dir.join("rootfs-x86_64.img"), b"rootfs").unwrap();
        write_runtime_qemu_template(root.path());
        let template = root.path().join("qemu-x86_64.toml");
        fs::write(
            &template,
            r#"
args = ["-nographic", "-drive", "id=disk0,if=none,format=raw,file=${workspace}/target/x86_64-unknown-none/rootfs-x86_64.img"]
uefi = false
to_bin = false
success_regex = ["ok"]
fail_regex = ["failed"]
shell_prefix = "starry:~#"
"#,
        )
        .unwrap();

        let request = ResolvedStarryRequest {
            package: "starryos".to_string(),
            arch: "x86_64".to_string(),
            target: "x86_64-unknown-none".to_string(),
            plat_dyn: None,
            debug: false,
            build_info_path: PathBuf::from("/tmp/.build.toml"),
            build_info_override: None,
            qemu_config: None,
            uboot_config: None,
        };

        let qemu = prepare_test_qemu_run_config(root.path(), &request, &template, None)
            .await
            .unwrap();

        assert_eq!(
            qemu.qemu_config,
            Some(root.path().join("os/StarryOS/starryos/.qemu-aarch64.toml"))
        );
        assert_eq!(qemu.timeout_seconds, None);
        assert!(
            qemu.override_args
                .args
                .as_ref()
                .unwrap()
                .iter()
                .any(|arg| arg.contains("disk-test-"))
        );
        assert!(
            !qemu.override_args.args.as_ref().unwrap().iter().any(
                |arg| arg.contains("${workspace}/target/x86_64-unknown-none/rootfs-x86_64.img")
            )
        );
        assert_eq!(
            qemu.override_args.shell_prefix.as_deref(),
            Some("starry:~#")
        );
    }

    #[test]
    fn apply_timeout_override_removes_timeout() {
        let mut config = QemuConfig {
            timeout: Some(3),
            ..Default::default()
        };

        apply_timeout_override(&mut config, Some(0));

        assert_eq!(config.timeout, None);
    }

    #[test]
    fn apply_timeout_override_replaces_existing_timeout() {
        let mut config = QemuConfig {
            timeout: Some(3),
            ..Default::default()
        };

        apply_timeout_override(&mut config, Some(10));

        assert_eq!(config.timeout, Some(10));
    }

    #[test]
    fn replace_workspace_variables_expands_workspace_placeholders() {
        let root = tempdir().unwrap();
        let mut config = QemuConfig {
            args: vec!["${workspace}/disk.img".to_string()],
            success_regex: vec!["${workspaceFolder}/ok".to_string()],
            fail_regex: vec!["${workspace}/fail".to_string()],
            shell_prefix: Some("${workspace}/prompt".to_string()),
            shell_init_cmd: Some("cat ${workspaceFolder}/init".to_string()),
            ..Default::default()
        };

        replace_workspace_variables(&mut config, root.path());

        let workspace = root.path().display().to_string();
        let expected_shell_prefix = format!("{workspace}/prompt");
        let expected_shell_init_cmd = format!("cat {workspace}/init");
        assert_eq!(config.args, vec![format!("{workspace}/disk.img")]);
        assert_eq!(config.success_regex, vec![format!("{workspace}/ok")]);
        assert_eq!(config.fail_regex, vec![format!("{workspace}/fail")]);
        assert_eq!(
            config.shell_prefix.as_deref(),
            Some(expected_shell_prefix.as_str())
        );
        assert_eq!(
            config.shell_init_cmd.as_deref(),
            Some(expected_shell_init_cmd.as_str())
        );
    }

    #[test]
    fn finalize_qemu_case_run_reports_case_names() {
        let err = finalize_qemu_case_run(
            &["smoke".to_string(), "stress-ng-0".to_string()],
            StarryTestGroup::Stress,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("starry stress qemu tests failed for 2 case(s): smoke, stress-ng-0")
        );
    }

    #[test]
    fn shared_rootfs_image_path_uses_workspace_placeholder() {
        let path = shared_rootfs_image_path("x86_64-unknown-none", "x86_64").unwrap();
        assert_eq!(
            path,
            "${workspace}/target/x86_64-unknown-none/rootfs-x86_64.img"
        );
    }

    #[test]
    fn finalize_qemu_test_helper_remains_available_for_package_flows() {
        assert!(test_qemu::finalize_qemu_test_run("arceos", &[]).is_ok());
    }
}
