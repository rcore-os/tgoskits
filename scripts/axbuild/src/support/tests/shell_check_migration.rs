use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[test]
fn tracked_toml_files_do_not_use_legacy_root_shell_check_fields() {
    let workspace_root = workspace_root();
    let mut violations = Vec::new();

    for relative_path in tracked_toml_paths(&workspace_root) {
        let path = workspace_root.join(&relative_path);
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        let document = toml::from_str::<toml::Value>(&source).unwrap_or_else(|error| {
            panic!("failed to parse tracked TOML {}: {error}", path.display())
        });
        let Some(root) = document.as_table() else {
            continue;
        };

        for field in ["shell_prefix", "shell_init_cmd", "success_regex"] {
            if root.contains_key(field) {
                violations.push(format!("{}: root `{field}`", relative_path.display()));
            }
        }
        if root.contains_key("test_commands") && root.contains_key("shell_check_steps") {
            violations.push(format!(
                "{}: combines root `test_commands` and `shell_check_steps`",
                relative_path.display()
            ));
        }
        if let Some(steps) = root
            .get("shell_check_steps")
            .and_then(toml::Value::as_array)
            && let Some(final_step) = steps.last().and_then(toml::Value::as_table)
            && final_step.contains_key("shell_cmd")
            && !final_step.contains_key("success_regex")
            && !final_step.contains_key("fail_regex")
        {
            violations.push(format!(
                "{}: final command step has no completion condition",
                relative_path.display()
            ));
        }
    }

    violations.sort();
    assert!(
        violations.is_empty(),
        "tracked TOML shell-check migration violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn interactive_app_configs_preserve_initialization_through_profile_autorun() {
    let workspace_root = workspace_root();

    for (config_path, boot_flag, prebuild_path, profile_name) in [
        (
            "apps/starry/deepseek-tui/qemu-x86_64-shell.toml",
            "starry.interactive=deepseek",
            "apps/starry/deepseek-tui/prebuild.sh",
            "deepseek-interactive.sh",
        ),
        (
            "apps/starry/mysql/qemu-x86_64-interactive.toml",
            "starry.interactive=mysql",
            "apps/starry/mysql/prebuild.sh",
            "mysql-interactive.sh",
        ),
        (
            "apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-interactive.toml",
            "starry.interactive=picoclaw",
            "apps/starry/picoclaw-cli/prebuild.sh",
            "picoclaw-interactive.sh",
        ),
    ] {
        let config_source = fs::read_to_string(workspace_root.join(config_path)).unwrap();
        let config: toml::Value = toml::from_str(&config_source).unwrap();
        let args = config["args"].as_array().unwrap();
        assert!(
            args.iter()
                .any(|arg| arg.as_str().is_some_and(|arg| arg.contains(boot_flag))),
            "{config_path} must select its profile autorun with `{boot_flag}`"
        );
        assert!(config.get("shell_check_steps").is_none(), "{config_path}");

        let prebuild_source = fs::read_to_string(workspace_root.join(prebuild_path)).unwrap();
        assert!(
            prebuild_source.contains(profile_name),
            "{prebuild_path} must install {profile_name}"
        );
    }
}

#[test]
fn command_only_ebpf_configs_preserve_autorun_without_shell_check_completion() {
    let workspace_root = workspace_root();

    for (app, command) in [
        ("kret", "/usr/bin/kret&"),
        ("mytrace", "/usr/bin/mytrace&"),
        ("rawtp", "/usr/bin/rawtp&"),
        ("upb", "/usr/bin/upb"),
        ("upb2", "/usr/bin/upb2"),
    ] {
        let app_dir = workspace_root.join("apps/starry/ebpf").join(app);
        let profile = fs::read_to_string(app_dir.join(format!("{app}-profile.sh"))).unwrap();
        assert_eq!(profile.trim(), command);
        let prebuild = fs::read_to_string(app_dir.join("prebuild.sh")).unwrap();
        assert!(prebuild.contains(&format!("{app}-profile.sh")));

        for arch in ["aarch64", "riscv64", "x86_64"] {
            let config: toml::Value = toml::from_str(
                &fs::read_to_string(app_dir.join(format!("qemu-{arch}.toml"))).unwrap(),
            )
            .unwrap();
            assert!(config.get("shell_check_steps").is_none(), "{app}/{arch}");
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("axbuild must live under scripts/axbuild")
        .to_path_buf()
}

fn tracked_toml_paths(workspace_root: &Path) -> Vec<PathBuf> {
    let output = Command::new("git")
        .current_dir(workspace_root)
        .args(["ls-files", "-z", "--", "*.toml"])
        .output()
        .expect("git must list tracked TOML files");
    assert!(
        output.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(String::from_utf8_lossy(path).into_owned()))
        .collect()
}
