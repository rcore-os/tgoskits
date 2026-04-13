use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use tokio::fs as tokio_fs;

use super::{board, rootfs};
use crate::context::{
    ResolvedStarryRequest, arch_for_target_checked, starry_target_for_arch_checked,
};

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
) -> anyhow::Result<Vec<StarryQemuCase>> {
    let test_suite_dir = test_suite_dir(workspace_root);
    let config_name = qemu_config_name(arch);

    if let Some(case_name) = selected_case {
        let case_dir = test_suite_dir.join(case_name);
        if !case_dir.is_dir() {
            bail!(
                "unknown Starry test case `{case_name}` in {}; available cases are discovered \
                 from direct subdirectories",
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
            "no Starry qemu test cases for arch `{arch}` found under {}",
            test_suite_dir.display()
        );
    }

    Ok(cases)
}

pub(crate) async fn prepare_test_qemu_config(
    workspace_root: &Path,
    request: &ResolvedStarryRequest,
    template_path: &Path,
    timeout_override: Option<u64>,
) -> anyhow::Result<PathBuf> {
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

    let shared_disk = shared_rootfs_image_path(&request.target, &request.arch)?;
    let config = tokio_fs::read_to_string(template_path)
        .await
        .with_context(|| format!("failed to read {}", template_path.display()))?;
    let config = config.replace(&shared_disk, &isolated_disk_img.display().to_string());
    let config = match timeout_override {
        None => config,
        Some(0) => remove_timeout_field(&config),
        Some(seconds) => update_timeout_field(&config, seconds),
    };

    let generated_config = std::env::temp_dir().join(format!(
        "starry-test-qemu-{}-{}-{}.toml",
        request.arch,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system time is before unix epoch")?
            .as_nanos()
    ));
    tokio_fs::write(&generated_config, config)
        .await
        .with_context(|| format!("failed to write {}", generated_config.display()))?;

    Ok(generated_config)
}

pub(crate) fn finalize_qemu_case_run(failed: &[String]) -> anyhow::Result<()> {
    if failed.is_empty() {
        println!("all starry qemu test cases passed");
        Ok(())
    } else {
        bail!(
            "starry qemu tests failed for {} case(s): {}",
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

fn test_suite_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join("test-suit").join("starryos")
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

fn remove_timeout_field(config: &str) -> String {
    if !config.contains("timeout") {
        return config.to_string();
    }

    config
        .lines()
        .filter(|line| !line.trim().starts_with("timeout"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn update_timeout_field(config: &str, timeout_seconds: u64) -> String {
    let timeout_line = format!("timeout = {}", timeout_seconds);
    if config.contains("timeout") {
        config
            .lines()
            .map(|line| {
                if line.trim().starts_with("timeout") {
                    timeout_line.clone()
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        format!("{config}\n{timeout_line}")
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

    fn write_case(root: &Path, case_name: &str, arch: &str, body: &str) {
        let case_dir = test_suite_dir(root).join(case_name);
        fs::create_dir_all(&case_dir).unwrap();
        fs::write(case_dir.join(qemu_config_name(arch)), body).unwrap();
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
    fn discovers_cases_in_lexicographic_order() {
        let root = tempdir().unwrap();
        write_case(root.path(), "stress-ng-0", "riscv64", "timeout = 10\n");
        write_case(root.path(), "smoke", "riscv64", "timeout = 5\n");
        write_case(root.path(), "board-only", "aarch64", "timeout = 5\n");

        let cases = discover_qemu_cases(root.path(), "riscv64", None).unwrap();
        assert_eq!(
            cases
                .iter()
                .map(|case| case.name.as_str())
                .collect::<Vec<_>>(),
            vec!["smoke", "stress-ng-0"]
        );
    }

    #[test]
    fn filters_to_selected_case() {
        let root = tempdir().unwrap();
        write_case(root.path(), "smoke", "riscv64", "timeout = 5\n");
        write_case(root.path(), "stress-ng-0", "riscv64", "timeout = 10\n");

        let cases = discover_qemu_cases(root.path(), "riscv64", Some("stress-ng-0")).unwrap();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].name, "stress-ng-0");
    }

    #[test]
    fn rejects_missing_selected_case() {
        let root = tempdir().unwrap();
        write_case(root.path(), "smoke", "riscv64", "timeout = 5\n");

        let err = discover_qemu_cases(root.path(), "riscv64", Some("missing")).unwrap_err();
        assert!(
            err.to_string()
                .contains("unknown Starry test case `missing`")
        );
    }

    #[test]
    fn rejects_selected_case_without_target_arch() {
        let root = tempdir().unwrap();
        write_case(root.path(), "smoke", "aarch64", "timeout = 5\n");

        let err = discover_qemu_cases(root.path(), "riscv64", Some("smoke")).unwrap_err();
        assert!(err.to_string().contains("does not provide"));
        assert!(err.to_string().contains("qemu-riscv64.toml"));
    }

    #[tokio::test]
    async fn prepare_test_qemu_config_rewrites_shared_disk_path() {
        let root = tempdir().unwrap();
        let target_dir = root.path().join("target/x86_64-unknown-none");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(target_dir.join("rootfs-x86_64.img"), b"rootfs").unwrap();
        let template = root.path().join("qemu-x86_64.toml");
        fs::write(
            &template,
            r#"
args = ["-nographic", "-drive", "id=disk0,if=none,format=raw,file=${workspace}/target/x86_64-unknown-none/rootfs-x86_64.img"]
shell_prefix = "starry:~#"
"#,
        )
        .unwrap();

        let request = ResolvedStarryRequest {
            package: "starryos".to_string(),
            arch: "x86_64".to_string(),
            target: "x86_64-unknown-none".to_string(),
            plat_dyn: None,
            build_info_path: PathBuf::from("/tmp/.build.toml"),
            qemu_config: None,
            uboot_config: None,
        };

        let generated = prepare_test_qemu_config(root.path(), &request, &template, None)
            .await
            .unwrap();
        let content = fs::read_to_string(generated).unwrap();

        assert!(content.contains("disk-test-"));
        assert!(!content.contains("${workspace}/target/x86_64-unknown-none/rootfs-x86_64.img"));
        assert!(content.contains("shell_prefix = \"starry:~#\""));
    }

    #[test]
    fn remove_timeout_field_removes_timeout_line() {
        let config = r#"args = ["-nographic"]
shell_prefix = "starry:~#"
timeout = 3
"#;
        let result = remove_timeout_field(config);
        assert!(!result.contains("timeout"));
        assert!(result.contains("args = [\"-nographic\"]"));
    }

    #[test]
    fn update_timeout_field_replaces_existing_timeout() {
        let config = r#"args = ["-nographic"]
timeout = 3
"#;
        let result = update_timeout_field(config, 10);
        assert!(result.contains("timeout = 10"));
        assert!(!result.contains("timeout = 3"));
    }

    #[test]
    fn finalize_qemu_case_run_reports_case_names() {
        let err =
            finalize_qemu_case_run(&["smoke".to_string(), "stress-ng-0".to_string()]).unwrap_err();
        assert!(
            err.to_string()
                .contains("starry qemu tests failed for 2 case(s): smoke, stress-ng-0")
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
