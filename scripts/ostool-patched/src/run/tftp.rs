//! TFTP server helpers for network booting.
//!
//! On Linux, this module prepares a system `tftpd-hpa` installation and stages
//! build artifacts into the configured TFTP root. Other platforms keep using
//! the built-in Rust TFTP server.

use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    net::{IpAddr, Ipv4Addr},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, anyhow, bail};
use colored::Colorize as _;
use tftpd::{Config, Server};

use crate::{Tool, utils::PathResultExt};

const TFTP_HPA_CONFIG_PATH: &str = "/etc/default/tftpd-hpa";
const DEFAULT_TFTP_DIRECTORY: &str = "/srv/tftp";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxTftpPrepared {
    pub tftp_root: PathBuf,
    pub target_dir: PathBuf,
    pub absolute_fit_path: PathBuf,
    pub relative_filename: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TftpdHpaConfig {
    pub username: Option<String>,
    pub directory: PathBuf,
    pub address: Option<String>,
    pub options: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DistroKind {
    Debian,
    Rhel,
    Arch,
    OpenSuse,
    Alpine,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandSpec {
    program: String,
    args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstallPlan {
    distro: DistroKind,
    commands: Vec<CommandSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EffectiveUser {
    name: String,
    group: String,
}

#[cfg(target_os = "linux")]
pub fn ensure_linux_tftpd_hpa() -> anyhow::Result<TftpdHpaConfig> {
    let binary = find_tftpd_binary();
    let is_root = is_root_user()?;

    if let Some(path) = binary {
        info!("Using system tftpd-hpa binary: {}", path.display());
    } else {
        let distro = detect_distro_kind()?;
        let install_plan = build_install_plan(distro)?;

        println!("{}", "未检测到 tftpd-hpa (in.tftpd)".yellow());
        println!("发行版: {}", distro.label());
        println!(
            "当前用户是否为 root: {}",
            if is_root { "yes" } else { "no" }
        );

        if install_plan.commands.is_empty() {
            bail!(
                "当前发行版暂不支持自动安装 tftpd-hpa，请手动安装后重试（发行版: {}）",
                distro.label()
            );
        }

        let display = render_command_chain(&install_plan.commands, is_root);
        println!("将执行安装命令:");
        println!("  {display}");

        if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
            bail!("当前终端不是交互式终端，请手动执行上述命令安装 tftpd-hpa");
        }

        if !prompt_yes_no("是否继续安装 tftpd-hpa? [y/N] ")? {
            bail!("已取消安装 tftpd-hpa");
        }

        for command in &install_plan.commands {
            run_privileged_command(command, is_root)
                .with_context(|| format!("failed to install tftpd-hpa via `{display}`"))?;
        }

        let path = find_tftpd_binary()
            .ok_or_else(|| anyhow!("安装完成后仍未找到 in.tftpd，请确认 tftpd-hpa 是否安装成功"))?;
        info!("Installed system tftpd-hpa binary: {}", path.display());
    }

    let (config, created) = ensure_tftpd_hpa_config(Path::new(TFTP_HPA_CONFIG_PATH), is_root)?;
    if created {
        if command_exists("systemctl") {
            let restart = CommandSpec {
                program: "systemctl".into(),
                args: vec!["restart".into(), "tftpd-hpa".into()],
            };
            run_privileged_command(&restart, is_root)
                .context("failed to restart tftpd-hpa after creating default config")?;
        } else {
            println!(
                "{}",
                "已创建 /etc/default/tftpd-hpa，请手动重启 tftpd-hpa 服务".yellow()
            );
        }
    }

    ensure_tftpd_hpa_service_ready(is_root)?;

    Ok(config)
}

#[cfg(target_os = "linux")]
pub fn stage_linux_fit_image(
    fitimage: &Path,
    tftp_root: &Path,
) -> anyhow::Result<LinuxTftpPrepared> {
    let prepared = prepare_linux_tftp_paths(fitimage, tftp_root)?;
    ensure_tftp_target_dir(&prepared.target_dir)?;
    fs::copy(fitimage, &prepared.absolute_fit_path).with_path("failed to copy file", fitimage)?;
    Ok(prepared)
}

pub fn relative_tftp_filename(fitimage: &Path) -> anyhow::Result<String> {
    let artifact_dir = fitimage
        .parent()
        .ok_or_else(|| anyhow!("invalid FIT image path: {}", fitimage.display()))?;
    let relative_path = relative_tftp_directory(artifact_dir)?.join(
        fitimage
            .file_name()
            .ok_or_else(|| anyhow!("invalid FIT image filename: {}", fitimage.display()))?,
    );
    Ok(relative_path.to_string_lossy().replace('\\', "/"))
}

/// Starts a built-in TFTP server serving files from the build output directory.
pub fn run_tftp_server(tool: &Tool) -> anyhow::Result<()> {
    let mut file_dir = tool.manifest_dir().clone();
    if let Some(elf_path) = &tool.ctx().artifacts.elf {
        file_dir = elf_path
            .parent()
            .ok_or(anyhow!("{} no parent dir", elf_path.display()))?
            .to_path_buf();
    }

    info!(
        "Starting TFTP server serving files from: {}",
        file_dir.display()
    );

    let mut config = Config::default();
    config.directory = file_dir;
    config.send_directory = config.directory.clone();
    config.port = 69;
    config.ip_address = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

    std::thread::spawn(move || {
        let mut server = Server::new(&config)
            .inspect_err(|e| {
                println!("{e}");
                println!("{}", "TFTP server 启动失败：若权限不足，尝试执行 `sudo setcap cap_net_bind_service=+eip $(which cargo-osrun)&&sudo setcap cap_net_bind_service=+eip $(which ostool)` 并重启终端".red());
                std::process::exit(1);
            })
            .unwrap();
        server.listen();
    });

    Ok(())
}

fn find_tftpd_binary() -> Option<PathBuf> {
    find_command_path("in.tftpd").or_else(|| {
        [
            "/usr/sbin/in.tftpd",
            "/sbin/in.tftpd",
            "/usr/bin/in.tftpd",
            "/usr/sbin/tftpd",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
    })
}

fn find_command_path(program: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

fn command_exists(program: &str) -> bool {
    find_command_path(program).is_some()
}

fn prompt_yes_no(prompt: &str) -> anyhow::Result<bool> {
    print!("{prompt}");
    io::stdout().flush().context("failed to flush stdout")?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .context("failed to read user input")?;
    let answer = answer.trim().to_ascii_lowercase();
    Ok(matches!(answer.as_str(), "y" | "yes"))
}

fn detect_distro_kind() -> anyhow::Result<DistroKind> {
    let os_release = fs::read_to_string("/etc/os-release")
        .context("failed to read /etc/os-release for distro detection")?;
    Ok(DistroKind::from_os_release(&os_release))
}

fn build_install_plan(distro: DistroKind) -> anyhow::Result<InstallPlan> {
    let commands = match distro {
        DistroKind::Debian => vec![
            CommandSpec {
                program: "apt-get".into(),
                args: vec!["update".into()],
            },
            CommandSpec {
                program: "apt-get".into(),
                args: vec!["install".into(), "-y".into(), "tftpd-hpa".into()],
            },
        ],
        DistroKind::Rhel => {
            let package_manager = if command_exists("dnf") { "dnf" } else { "yum" };
            vec![CommandSpec {
                program: package_manager.into(),
                args: vec!["install".into(), "-y".into(), "tftp-server".into()],
            }]
        }
        DistroKind::Arch => vec![CommandSpec {
            program: "pacman".into(),
            args: vec!["-Sy".into(), "--noconfirm".into(), "tftp-hpa".into()],
        }],
        DistroKind::OpenSuse => vec![CommandSpec {
            program: "zypper".into(),
            args: vec!["install".into(), "-y".into(), "tftp".into()],
        }],
        DistroKind::Alpine => vec![CommandSpec {
            program: "apk".into(),
            args: vec!["add".into(), "tftp-hpa".into()],
        }],
        DistroKind::Unsupported => vec![],
    };

    Ok(InstallPlan { distro, commands })
}

fn render_command_chain(commands: &[CommandSpec], is_root: bool) -> String {
    commands
        .iter()
        .map(|command| render_command(command, is_root))
        .collect::<Vec<_>>()
        .join(" && ")
}

fn render_command(command: &CommandSpec, is_root: bool) -> String {
    let mut parts = Vec::with_capacity(command.args.len() + 2);
    if !is_root {
        parts.push("sudo".to_string());
    }
    parts.push(command.program.clone());
    parts.extend(command.args.clone());
    parts.join(" ")
}

fn run_privileged_command(command: &CommandSpec, is_root: bool) -> anyhow::Result<()> {
    eprintln!("{}", render_command(command, is_root).purple());
    let mut process = if is_root {
        let mut process = Command::new(&command.program);
        process.args(&command.args);
        process
    } else {
        let mut process = Command::new("sudo");
        process.arg(&command.program).args(&command.args);
        process
    };

    let status = process
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to start `{}`", command.program))?;

    if status.success() {
        Ok(())
    } else {
        bail!("command `{}` exited with status {status}", command.program)
    }
}

fn run_capture(program: &str, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to execute `{program}`"))?;

    if !output.status.success() {
        bail!("command `{program}` exited with status {}", output.status);
    }

    let text = String::from_utf8(output.stdout)
        .with_context(|| format!("failed to decode output from `{program}`"))?;
    Ok(text.trim().to_string())
}

fn is_root_user() -> anyhow::Result<bool> {
    Ok(run_capture("id", &["-u"])? == "0")
}

fn ensure_tftpd_hpa_service_ready(is_root: bool) -> anyhow::Result<()> {
    if udp_port_69_is_listening()? {
        info!("tftpd-hpa is already listening on UDP port 69");
        return Ok(());
    }

    if command_exists("systemctl") {
        println!(
            "{}",
            "tftpd-hpa 当前未监听 UDP 69，正在尝试启动/重启服务".yellow()
        );
        let restart = CommandSpec {
            program: "systemctl".into(),
            args: vec!["restart".into(), "tftpd-hpa".into()],
        };
        run_privileged_command(&restart, is_root).context("failed to restart tftpd-hpa service")?;

        if udp_port_69_is_listening()? {
            info!("tftpd-hpa is now listening on UDP port 69");
            return Ok(());
        }

        let active = run_capture("systemctl", &["is-active", "tftpd-hpa"])
            .unwrap_or_else(|_| "unknown".to_string());
        bail!("tftpd-hpa 服务重启后仍未监听 UDP 69（systemctl is-active: {active}）");
    }

    bail!("未检测到可用的服务管理器，且 tftpd-hpa 当前未监听 UDP 69，请手动启动服务");
}

fn udp_port_69_is_listening() -> anyhow::Result<bool> {
    let output = run_capture("ss", &["-lun"])?;
    Ok(ss_output_has_udp_port_69(&output))
}

fn ss_output_has_udp_port_69(output: &str) -> bool {
    output.lines().any(|line| {
        let line = line.trim();
        !line.is_empty()
            && !line.starts_with("State")
            && line.split_whitespace().any(|field| {
                field.ends_with(":69")
                    || field.ends_with(":69,")
                    || field.ends_with("]:69")
                    || field == "*:69"
                    || field == "0.0.0.0:69"
                    || field == "[::]:69"
            })
    })
}

fn ensure_tftpd_hpa_config(path: &Path, is_root: bool) -> anyhow::Result<(TftpdHpaConfig, bool)> {
    if path.exists() {
        let content = fs::read_to_string(path).with_path("failed to read file", path)?;
        let config = TftpdHpaConfig::parse(&content)?;
        return Ok((config, false));
    }

    let content = TftpdHpaConfig::render_default();
    write_root_owned_file(path, &content, is_root)?;
    let config = TftpdHpaConfig::parse(&content)?;
    Ok((config, true))
}

fn write_root_owned_file(path: &Path, content: &str, is_root: bool) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path {} has no parent directory", path.display()))?;

    if is_root {
        fs::create_dir_all(parent).with_path("failed to create directory", parent)?;
        fs::write(path, content).with_path("failed to write file", path)?;
        return Ok(());
    }

    let temp_path = temp_file_path("ostool-tftpd-hpa");
    fs::write(&temp_path, content).with_path("failed to write temp file", &temp_path)?;

    let mkdir = CommandSpec {
        program: "mkdir".into(),
        args: vec!["-p".into(), parent.display().to_string()],
    };
    let copy = CommandSpec {
        program: "cp".into(),
        args: vec![temp_path.display().to_string(), path.display().to_string()],
    };

    let mkdir_result = run_privileged_command(&mkdir, false);
    let copy_result = run_privileged_command(&copy, false);
    let _ = fs::remove_file(&temp_path);

    mkdir_result?;
    copy_result?;
    Ok(())
}

fn temp_file_path(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    env::temp_dir().join(format!("{prefix}-{}-{nanos}.tmp", std::process::id()))
}

fn prepare_linux_tftp_paths(
    fitimage: &Path,
    tftp_root: &Path,
) -> anyhow::Result<LinuxTftpPrepared> {
    let relative_filename = relative_tftp_filename(fitimage)?;
    let relative_path = PathBuf::from(&relative_filename);
    let relative_dir = relative_path
        .parent()
        .ok_or_else(|| anyhow!("invalid relative TFTP path: {}", relative_path.display()))?
        .to_path_buf();
    let target_dir = tftp_root.join(&relative_dir);
    let absolute_fit_path = tftp_root.join(&relative_path);

    Ok(LinuxTftpPrepared {
        tftp_root: tftp_root.to_path_buf(),
        target_dir,
        absolute_fit_path,
        relative_filename,
    })
}

fn relative_tftp_directory(artifact_dir: &Path) -> anyhow::Result<PathBuf> {
    if !artifact_dir.is_absolute() {
        bail!(
            "artifact directory must be absolute for Linux system TFTP: {}",
            artifact_dir.display()
        );
    }

    let mut relative = PathBuf::from("ostool");
    for component in artifact_dir.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                bail!(
                    "artifact directory must not contain parent segments: {}",
                    artifact_dir.display()
                )
            }
            Component::Prefix(prefix) => relative.push(prefix.as_os_str()),
        }
    }
    Ok(relative)
}

fn ensure_tftp_target_dir(target_dir: &Path) -> anyhow::Result<()> {
    match fs::create_dir_all(target_dir) {
        Ok(()) => return Ok(()),
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {}
        Err(err) => return Err(err).with_path("failed to create directory", target_dir),
    }

    let user = effective_user()?;
    let mkdir = CommandSpec {
        program: "mkdir".into(),
        args: vec!["-p".into(), target_dir.display().to_string()],
    };
    run_privileged_command(&mkdir, false)
        .with_context(|| format!("failed to create directory {}", target_dir.display()))?;

    let chown = CommandSpec {
        program: "chown".into(),
        args: vec![
            "-R".into(),
            format!("{}:{}", user.name, user.group),
            target_dir.display().to_string(),
        ],
    };
    run_privileged_command(&chown, false)
        .with_context(|| format!("failed to change ownership for {}", target_dir.display()))?;
    Ok(())
}

fn effective_user() -> anyhow::Result<EffectiveUser> {
    let name = env::var("SUDO_USER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            env::var("USER")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or(run_capture("id", &["-un"])?);

    let group = run_capture("id", &["-gn", &name])?;
    Ok(EffectiveUser { name, group })
}

impl TftpdHpaConfig {
    fn parse(content: &str) -> anyhow::Result<Self> {
        let mut username = None;
        let mut directory = None;
        let mut address = None;
        let mut options = None;

        for line in content.lines() {
            let Some((key, value)) = parse_key_value(line) else {
                continue;
            };
            match key {
                "TFTP_USERNAME" => username = Some(value),
                "TFTP_DIRECTORY" => directory = Some(value),
                "TFTP_ADDRESS" => address = Some(value),
                "TFTP_OPTIONS" => options = Some(value),
                _ => {}
            }
        }

        let directory = directory
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("tftpd-hpa config is missing TFTP_DIRECTORY"))?;

        Ok(Self {
            username,
            directory: PathBuf::from(directory),
            address,
            options,
        })
    }

    fn render_default() -> String {
        format!(
            "TFTP_USERNAME=\"tftp\"\nTFTP_DIRECTORY=\"{DEFAULT_TFTP_DIRECTORY}\"\nTFTP_ADDRESS=\":69\"\nTFTP_OPTIONS=\"-l -s -c\"\n"
        )
    }
}

fn parse_key_value(line: &str) -> Option<(&str, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (key, value) = trimmed.split_once('=')?;
    Some((key.trim(), unquote(value.trim())))
}

fn unquote(value: &str) -> String {
    let mut chars = value.chars();
    match (chars.next(), value.chars().last()) {
        (Some('"'), Some('"')) | (Some('\''), Some('\'')) if value.len() >= 2 => {
            value[1..value.len() - 1].to_string()
        }
        _ => value.to_string(),
    }
}

impl DistroKind {
    fn from_os_release(content: &str) -> Self {
        let mut ids = Vec::new();

        for line in content.lines() {
            let Some((key, value)) = parse_key_value(line) else {
                continue;
            };
            match key {
                "ID" => ids.push(value),
                "ID_LIKE" => ids.extend(value.split_whitespace().map(ToOwned::to_owned)),
                _ => {}
            }
        }

        if ids
            .iter()
            .any(|id| matches!(id.as_str(), "debian" | "ubuntu"))
        {
            return Self::Debian;
        }
        if ids.iter().any(|id| {
            matches!(
                id.as_str(),
                "fedora" | "rhel" | "centos" | "rocky" | "almalinux"
            )
        }) {
            return Self::Rhel;
        }
        if ids
            .iter()
            .any(|id| matches!(id.as_str(), "arch" | "archlinux" | "manjaro"))
        {
            return Self::Arch;
        }
        if ids.iter().any(|id| {
            matches!(
                id.as_str(),
                "opensuse" | "opensuse-tumbleweed" | "sles" | "suse"
            )
        }) {
            return Self::OpenSuse;
        }
        if ids.iter().any(|id| id == "alpine") {
            return Self::Alpine;
        }

        Self::Unsupported
    }

    fn label(self) -> &'static str {
        match self {
            Self::Debian => "debian/ubuntu",
            Self::Rhel => "rhel/fedora",
            Self::Arch => "arch",
            Self::OpenSuse => "opensuse/sles",
            Self::Alpine => "alpine",
            Self::Unsupported => "unsupported",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distro_detection_uses_id_and_id_like() {
        let ubuntu = r#"
ID=ubuntu
ID_LIKE=debian
"#;
        let rocky = r#"
ID=rocky
ID_LIKE="rhel fedora"
"#;
        let arch = "ID=manjaro\nID_LIKE=arch\n";

        assert_eq!(DistroKind::from_os_release(ubuntu), DistroKind::Debian);
        assert_eq!(DistroKind::from_os_release(rocky), DistroKind::Rhel);
        assert_eq!(DistroKind::from_os_release(arch), DistroKind::Arch);
    }

    #[test]
    fn render_command_chain_adds_sudo_for_non_root() {
        let commands = vec![
            CommandSpec {
                program: "apt-get".into(),
                args: vec!["update".into()],
            },
            CommandSpec {
                program: "apt-get".into(),
                args: vec!["install".into(), "-y".into(), "tftpd-hpa".into()],
            },
        ];

        assert_eq!(
            render_command_chain(&commands, false),
            "sudo apt-get update && sudo apt-get install -y tftpd-hpa"
        );
        assert_eq!(
            render_command_chain(&commands, true),
            "apt-get update && apt-get install -y tftpd-hpa"
        );
    }

    #[test]
    fn default_tftpd_hpa_config_matches_plan() {
        assert_eq!(
            TftpdHpaConfig::render_default(),
            "TFTP_USERNAME=\"tftp\"\nTFTP_DIRECTORY=\"/srv/tftp\"\nTFTP_ADDRESS=\":69\"\nTFTP_OPTIONS=\"-l -s -c\"\n"
        );
    }

    #[test]
    fn parse_existing_tftpd_hpa_directory() {
        let config = TftpdHpaConfig::parse(
            r#"
TFTP_USERNAME="tftp"
TFTP_DIRECTORY="/mnt/d/tftpboot/"
TFTP_ADDRESS=":69"
TFTP_OPTIONS="-l -s -c"
"#,
        )
        .unwrap();

        assert_eq!(config.directory, PathBuf::from("/mnt/d/tftpboot/"));
        assert_eq!(config.options.as_deref(), Some("-l -s -c"));
    }

    #[test]
    fn relative_filename_keeps_absolute_artifact_hierarchy() {
        let fitimage =
            Path::new("/home/zhourui/opensource/tgoskits2/target/aarch64/release/image.fit");
        let prepared = prepare_linux_tftp_paths(fitimage, Path::new("/srv/tftp")).unwrap();

        assert_eq!(
            prepared.relative_filename,
            "ostool/home/zhourui/opensource/tgoskits2/target/aarch64/release/image.fit"
        );
        assert_eq!(
            prepared.target_dir,
            PathBuf::from(
                "/srv/tftp/ostool/home/zhourui/opensource/tgoskits2/target/aarch64/release"
            )
        );
        assert_eq!(
            prepared.absolute_fit_path,
            PathBuf::from(
                "/srv/tftp/ostool/home/zhourui/opensource/tgoskits2/target/aarch64/release/image.fit"
            )
        );
    }

    #[test]
    fn relative_tftp_filename_keeps_ostool_prefix_for_existing_tftp_root() {
        let fitimage =
            Path::new("/home/zhourui/opensource/tgoskits2/target/aarch64/release/image.fit");

        assert_eq!(
            relative_tftp_filename(fitimage).unwrap(),
            "ostool/home/zhourui/opensource/tgoskits2/target/aarch64/release/image.fit"
        );
    }

    #[test]
    fn ss_port_detection_matches_udp_69_listener() {
        let output = "\
State  Recv-Q Send-Q Local Address:Port Peer Address:PortProcess\n\
UNCONN 0      0      0.0.0.0:69      0.0.0.0:*\n";

        assert!(ss_output_has_udp_port_69(output));
        assert!(!ss_output_has_udp_port_69(
            "State Recv-Q Send-Q Local Address:Port Peer Address:PortProcess\n"
        ));
    }
}
