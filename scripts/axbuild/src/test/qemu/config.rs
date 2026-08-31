use super::{discovery::qemu_configs_in_dir, *};

pub(crate) fn normalize_qemu_test_commands<I, S>(
    qemu_config_path: &Path,
    commands: I,
    suite_name: &str,
) -> anyhow::Result<Vec<String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut test_commands = Vec::new();
    for command in commands {
        let command = command.as_ref().trim().to_string();
        if command.is_empty() {
            bail!(
                "{suite_name} grouped qemu case `{}` contains an empty test command",
                qemu_config_path.display()
            );
        }
        test_commands.push(command);
    }
    Ok(test_commands)
}

pub(crate) fn load_test_qemu_case_fields(
    display_name: String,
    name: String,
    case_dir: PathBuf,
    qemu_config_path: PathBuf,
    suite_name: &str,
    discover_subcases: bool,
) -> anyhow::Result<TestQemuCase> {
    let (test_case, write_policy) = load_qemu_case_fields_with_write_policy(
        display_name,
        name,
        case_dir,
        qemu_config_path,
        suite_name,
        discover_subcases,
    )?;
    ensure_test_rootfs_write_policy(write_policy, &test_case.qemu_config_path, suite_name)?;
    Ok(test_case)
}

pub(crate) fn validate_test_qemu_rootfs_write_policy(
    qemu_config_path: &Path,
    suite_name: &str,
) -> anyhow::Result<()> {
    let config = load_qemu_case_extra_config(qemu_config_path)?;
    ensure_test_rootfs_write_policy(config.rootfs_write_policy, qemu_config_path, suite_name)
}

fn ensure_test_rootfs_write_policy(
    write_policy: crate::rootfs::qemu::RootfsWritePolicy,
    qemu_config_path: &Path,
    suite_name: &str,
) -> anyhow::Result<()> {
    if write_policy == crate::rootfs::qemu::RootfsWritePolicy::Persist {
        bail!(
            "{suite_name} qemu test case `{}` cannot use `rootfs_write_policy = \"persist\"`; \
             test rootfs writes must be discarded",
            qemu_config_path.display()
        );
    }
    Ok(())
}

pub(crate) fn load_qemu_case_fields_with_write_policy(
    display_name: String,
    name: String,
    case_dir: PathBuf,
    qemu_config_path: PathBuf,
    suite_name: &str,
    discover_subcases: bool,
) -> anyhow::Result<(TestQemuCase, crate::rootfs::qemu::RootfsWritePolicy)> {
    let config = load_qemu_case_extra_config(&qemu_config_path)?;
    let write_policy = config.rootfs_write_policy;
    let test_commands =
        normalize_qemu_test_commands(&qemu_config_path, config.test_commands, suite_name)?;
    let subcases = if discover_subcases && !test_commands.is_empty() {
        let arch = qemu_config_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.strip_prefix("qemu-"));
        discover_qemu_subcases(&case_dir, arch)?
    } else {
        Vec::new()
    };
    let test_case = TestQemuCase {
        display_name,
        name,
        case_dir,
        qemu_config_path,
        test_commands,
        host_symbolize_success_regex: config.host_symbolize_success_regex,
        host_http_server: config.host_http_server,
        subcases,
        grouped_subcase_filter: None,
    };
    Ok((test_case, write_policy))
}

pub(crate) fn load_qemu_case_extra_config(
    qemu_config_path: &Path,
) -> anyhow::Result<QemuCaseExtraConfig> {
    let content = fs::read_to_string(qemu_config_path)
        .with_context(|| format!("failed to read {}", qemu_config_path.display()))?;
    let config: QemuCaseExtraConfig = toml::from_str(&content)
        .with_context(|| format!("failed to parse {}", qemu_config_path.display()))?;
    if config.legacy_snapshot.is_some() {
        bail!(
            "QEMU config `{}` uses removed field `snapshot`; replace it with `rootfs_write_policy \
             = \"discard\"` or `rootfs_write_policy = \"persist\"`",
            qemu_config_path.display()
        );
    }
    Ok(config)
}

pub(crate) fn load_qemu_case_host_http_server(
    qemu_config_path: &Path,
) -> anyhow::Result<Option<HostHttpServerConfig>> {
    Ok(load_qemu_case_extra_config(qemu_config_path)?.host_http_server)
}

pub(super) fn discover_qemu_subcases(
    case_dir: &Path,
    arch: Option<&str>,
) -> anyhow::Result<Vec<TestQemuSubcase>> {
    let mut subcases = Vec::new();
    for entry in
        fs::read_dir(case_dir).with_context(|| format!("failed to read {}", case_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        if let Some(arch) = arch
            && let Some(configs) = qemu_configs_in_dir(&path)?
            && !configs.contains_key(arch)
        {
            continue;
        }

        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let kind = if path.join("c").is_dir() || path.join("CMakeLists.txt").is_file() {
            Some(TestQemuSubcaseKind::C)
        } else if path.join("rust").is_dir() {
            Some(TestQemuSubcaseKind::Rust)
        } else {
            None
        };

        if let Some(kind) = kind {
            subcases.push(TestQemuSubcase {
                name,
                case_dir: path,
                kind,
            });
        }
    }
    subcases.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(subcases)
}

pub(crate) fn validate_grouped_qemu_commands(
    qemu: &QemuConfig,
    case: &TestQemuCase,
    suite_name: &str,
) -> anyhow::Result<()> {
    let shell_init_cmd_set = qemu
        .shell_init_cmd
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if shell_init_cmd_set && !case.test_commands.is_empty() {
        bail!(
            "{suite_name} grouped qemu case `{}` cannot define both `shell_init_cmd` and \
             `test_commands`",
            case.qemu_config_path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rootfs::qemu::RootfsWritePolicy;

    #[test]
    fn qemu_test_case_rejects_persistent_rootfs_policy() {
        let error = ensure_test_rootfs_write_policy(
            RootfsWritePolicy::Persist,
            Path::new("qemu-x86_64.toml"),
            "Starry",
        )
        .unwrap_err()
        .to_string();

        assert!(
            error.contains("test rootfs writes must be discarded"),
            "{error}"
        );
    }
}
