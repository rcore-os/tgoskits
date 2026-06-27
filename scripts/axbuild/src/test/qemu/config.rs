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
        let command = command
            .as_ref()
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .trim()
            .to_string();
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
    let config = load_qemu_case_extra_config(&qemu_config_path)?;
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
    Ok(TestQemuCase {
        display_name,
        name,
        case_dir,
        qemu_config_path,
        test_commands,
        host_symbolize_success_regex: config.host_symbolize_success_regex,
        host_http_server: config.host_http_server,
        subcases,
        grouped_subcase_filter: None,
    })
}

pub(crate) fn load_qemu_case_extra_config(
    qemu_config_path: &Path,
) -> anyhow::Result<QemuCaseExtraConfig> {
    let content = fs::read_to_string(qemu_config_path)
        .with_context(|| format!("failed to read {}", qemu_config_path.display()))?;
    toml::from_str(&content)
        .with_context(|| format!("failed to parse {}", qemu_config_path.display()))
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
