//! QEMU emulator runner with UEFI/OVMF support.
//!
//! This module provides functionality for running operating systems in QEMU
//! with support for:
//!
//! - Multiple architectures (x86_64, aarch64, riscv64, etc.)
//! - UEFI boot via OVMF firmware
//! - Debug mode with GDB server
//! - Output pattern matching for test automation
//!
//! # Configuration
//!
//! QEMU configuration is stored in `.qemu.toml` files:
//!
//! ```toml
//! args = ["-nographic", "-cpu", "cortex-a53"]
//! uefi = false
//! to_bin = true
//! success_regex = ["All tests passed"]
//! fail_regex = ["PANIC", "FAILED"]
//! ```

use std::{
    ffi::OsString,
    io::{self, ErrorKind},
    path::Path,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, anyhow};
#[cfg(windows)]
use colored::Colorize;
use object::Architecture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command as TokioCommand,
    sync::mpsc,
};

use crate::{
    Tool,
    build::config::Cargo,
    run::{
        output_matcher::{ByteStreamMatcher, compile_regexes, print_match_event},
        ovmf_prebuilt::{Arch, FileType, Prebuilt, Source},
        shell_init::{SHELL_INIT_DELAY, ShellAutoInitMatcher, normalize_shell_init_config},
    },
    sterm::{AsyncTerminal, TerminalConfig},
    utils::PathResultExt,
};

enum UefiBootConfig {
    Pflash {
        code: PathBuf,
        vars: PathBuf,
        esp_dir: PathBuf,
    },
}

/// QEMU configuration structure.
///
/// This configuration is typically loaded from a `.qemu.toml` file.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
pub struct QemuConfig {
    /// Additional QEMU command-line arguments.
    pub args: Vec<String>,
    /// Whether to use UEFI boot via OVMF firmware.
    pub uefi: bool,
    /// Whether to convert ELF to raw binary before loading.
    pub to_bin: bool,
    /// Regex patterns that indicate successful execution.
    pub success_regex: Vec<String>,
    /// Regex patterns that indicate failed execution.
    pub fail_regex: Vec<String>,
    /// String prefix that indicates the guest shell is ready.
    pub shell_prefix: Option<String>,
    /// Command sent once after `shell_prefix` is detected.
    pub shell_init_cmd: Option<String>,
    /// Timeout in seconds. `None` or `0` disables the timeout.
    pub timeout: Option<u64>,
}

impl QemuConfig {
    fn replace_strings(&mut self, tool: &Tool) -> anyhow::Result<()> {
        self.args = self
            .args
            .iter()
            .map(|arg| tool.replace_string(arg))
            .collect::<anyhow::Result<Vec<_>>>()?;
        self.success_regex = self
            .success_regex
            .iter()
            .map(|arg| tool.replace_string(arg))
            .collect::<anyhow::Result<Vec<_>>>()?;
        self.fail_regex = self
            .fail_regex
            .iter()
            .map(|arg| tool.replace_string(arg))
            .collect::<anyhow::Result<Vec<_>>>()?;
        self.shell_prefix = self
            .shell_prefix
            .as_deref()
            .map(|value| tool.replace_string(value))
            .transpose()?;
        self.shell_init_cmd = self
            .shell_init_cmd
            .as_deref()
            .map(|value| tool.replace_string(value))
            .transpose()?;
        Ok(())
    }

    fn normalize(&mut self, config_name: &str) -> anyhow::Result<()> {
        normalize_shell_init_config(
            &mut self.shell_prefix,
            &mut self.shell_init_cmd,
            config_name,
        )
    }

    fn shell_auto_init(&self) -> Option<ShellAutoInitMatcher> {
        ShellAutoInitMatcher::new(self.shell_prefix.clone(), self.shell_init_cmd.clone())
    }
}

/// Pure execution options for running an already prepared artifact in QEMU.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunQemuOptions {
    /// Whether to dump the device tree blob.
    pub dtb_dump: bool,
    /// Whether to show QEMU output.
    pub show_output: bool,
}

/// Runs the operating system in QEMU.
///
/// This function configures and launches QEMU with the appropriate settings
/// based on the detected architecture and configuration file.
///
/// # Arguments
///
/// * `tool` - The tool containing paths and build artifacts.
/// * `args` - QEMU run arguments.
///
/// # Errors
///
/// Returns an error if QEMU fails to start or exits with an error.
impl Tool {
    /// Returns the default QEMU runtime configuration for the current tool context.
    pub fn default_qemu_config(&self) -> QemuConfig {
        build_default_qemu_config(self.ctx.arch)
    }

    /// Returns the default QEMU runtime configuration for a Cargo build config.
    pub fn default_qemu_config_for_cargo(&self, cargo: &Cargo) -> QemuConfig {
        build_default_qemu_config(infer_target_arch(&cargo.target).or(self.ctx.arch))
    }

    pub async fn read_qemu_config_from_path_for_cargo(
        &mut self,
        cargo: &Cargo,
        path: &Path,
    ) -> anyhow::Result<QemuConfig> {
        self.sync_cargo_context(cargo);
        let config_path = self.replace_path_variables(path.to_path_buf())?;
        read_qemu_config_at_path(self, config_path).await
    }

    pub async fn ensure_qemu_config_for_cargo(
        &mut self,
        cargo: &Cargo,
    ) -> anyhow::Result<QemuConfig> {
        self.sync_cargo_context(cargo);
        let package_dir = self.resolve_package_manifest_dir(&cargo.package)?;
        let arch = infer_target_arch(&cargo.target).or(self.ctx.arch);
        let config_path = resolve_qemu_config_path_in_dir(&package_dir, arch, None)?;
        let default_config = self.default_qemu_config_for_cargo(cargo);
        ensure_qemu_config_at_path(self, config_path, default_config).await
    }

    pub async fn ensure_qemu_config_in_dir_for_cargo(
        &mut self,
        cargo: &Cargo,
        dir: &Path,
    ) -> anyhow::Result<QemuConfig> {
        self.sync_cargo_context(cargo);
        let dir = self.replace_path_variables(dir.to_path_buf())?;
        let arch = infer_target_arch(&cargo.target).or(self.ctx.arch);
        let config_path = resolve_qemu_config_path_in_dir(&dir, arch, None)?;
        let default_config = self.default_qemu_config_for_cargo(cargo);
        ensure_qemu_config_at_path(self, config_path, default_config).await
    }

    /// Loads a QEMU configuration from a directory using the default filename search.
    pub async fn ensure_qemu_config_in_dir(&mut self, dir: &Path) -> anyhow::Result<QemuConfig> {
        let dir = self.replace_path_variables(dir.to_path_buf())?;
        let config_path = resolve_qemu_config_path_in_dir(&dir, self.ctx.arch, None)?;
        let default_config = self.default_qemu_config();
        ensure_qemu_config_at_path(self, config_path, default_config).await
    }

    /// Reads a QEMU configuration from an explicit path without creating defaults.
    pub async fn read_qemu_config_from_path(&mut self, path: &Path) -> anyhow::Result<QemuConfig> {
        let config_path = self.replace_path_variables(path.to_path_buf())?;
        read_qemu_config_at_path(self, config_path).await
    }

    /// Runs an already prepared artifact in QEMU using a fully materialized configuration.
    pub async fn run_qemu(
        &mut self,
        config: &QemuConfig,
        options: RunQemuOptions,
    ) -> anyhow::Result<()> {
        let mut config = config.clone();
        config.replace_strings(self)?;
        config.normalize("QEMU runtime config")?;
        run_qemu_with_config(self, options, config).await
    }
}

async fn read_qemu_config_at_path(tool: &Tool, config_path: PathBuf) -> anyhow::Result<QemuConfig> {
    info!("Using QEMU config file: {}", config_path.display());

    let content = fs::read_to_string(&config_path)
        .await
        .with_context(|| format!("failed to read QEMU config: {}", config_path.display()))?;
    let mut config: QemuConfig = toml::from_str(&content)
        .with_context(|| format!("failed to parse QEMU config: {}", config_path.display()))?;
    config.replace_strings(tool)?;
    config.normalize(&format!("QEMU config {}", config_path.display()))?;
    Ok(config)
}

async fn ensure_qemu_config_at_path(
    tool: &Tool,
    config_path: PathBuf,
    default_config: QemuConfig,
) -> anyhow::Result<QemuConfig> {
    info!("Using QEMU config file: {}", config_path.display());

    let config_content = match fs::read_to_string(&config_path).await {
        Ok(_) => return read_qemu_config_at_path(tool, config_path).await,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let mut config = default_config;
            config.normalize(&format!("QEMU config {}", config_path.display()))?;
            fs::write(&config_path, toml::to_string_pretty(&config)?)
                .await
                .with_path("failed to write file", &config_path)?;
            config
        }
        Err(e) => return Err(e.into()),
    };
    Ok(config_content)
}

fn build_default_qemu_config(arch: Option<Architecture>) -> QemuConfig {
    let mut config = QemuConfig {
        to_bin: true,
        ..Default::default()
    };
    config.args.push("-nographic".to_string());
    if let Some(arch) = arch {
        match arch {
            Architecture::Aarch64 => {
                config.args.push("-cpu".to_string());
                config.args.push("cortex-a53".to_string());
            }
            Architecture::Riscv64 => {
                config.args.push("-cpu".to_string());
                config.args.push("rv64".to_string());
            }
            _ => {}
        }
    }
    config
}

pub(crate) fn infer_target_arch(target: &str) -> Option<Architecture> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }

    let triple = target.split('-').next().unwrap_or(target);
    match triple {
        "aarch64" => Some(Architecture::Aarch64),
        "arm" | "armv7" | "armv7a" | "armv7r" | "thumbv7em" => Some(Architecture::Arm),
        "riscv64" | "riscv64gc" => Some(Architecture::Riscv64),
        "x86_64" => Some(Architecture::X86_64),
        "i386" | "i586" | "i686" => Some(Architecture::I386),
        "loongarch64" => Some(Architecture::LoongArch64),
        _ => None,
    }
}

async fn run_qemu_with_config(
    tool: &mut Tool,
    run_args: RunQemuOptions,
    config: QemuConfig,
) -> anyhow::Result<()> {
    let mut runner = QemuRunner {
        tool,
        config,
        dtbdump: run_args.dtb_dump,
        success_regex: vec![],
        fail_regex: vec![],
    };
    runner.run().await
}

struct QemuRunner<'a> {
    tool: &'a mut Tool,
    config: QemuConfig,
    dtbdump: bool,
    success_regex: Vec<regex::Regex>,
    fail_regex: Vec<regex::Regex>,
}

impl QemuRunner<'_> {
    async fn run(&mut self) -> anyhow::Result<()> {
        self.prepare_regex()?;

        if self.config.to_bin {
            self.tool.objcopy_output_bin()?;
        }

        let detected_arch = self.tool.ctx.arch.ok_or_else(|| {
            anyhow!("Please specify `arch` in QEMU config or provide a valid ELF file.")
        })?;
        let arch = format!("{detected_arch:?}").to_lowercase();

        let machine = match detected_arch {
            Architecture::X86_64 | Architecture::I386 => "q35",
            _ => "virt",
        }
        .to_string();

        let mut need_machine = true;

        #[allow(unused_mut)]
        let mut qemu_executable = format!("qemu-system-{}", arch);

        #[cfg(windows)]
        {
            println!("{}", "Checking for QEMU executable on Windows...".blue());
            // Windows 特殊处理
            let msys2 =
                PathBuf::from("C:\\msys64\\ucrt64\\bin").join(format!("{qemu_executable}.exe"));

            if msys2.exists() {
                println!("Using QEMU executable from MSYS2: {}", msys2.display());
                qemu_executable = msys2.to_string_lossy().to_string();
            }
        }

        let mut cmd = self.tool.command(&qemu_executable);

        for arg in &self.config.args {
            if arg == "-machine" || arg == "-M" {
                need_machine = false;
            }
            cmd.arg(arg);
        }

        if self.dtbdump {
            let dtb_dump_path = PathBuf::from("target/qemu.dtb");
            if let Err(err) = fs::remove_file(&dtb_dump_path).await
                && err.kind() != ErrorKind::NotFound
            {
                return Err(err).with_path("failed to remove file", &dtb_dump_path);
            }
            cmd.arg("-machine")
                .arg(format!("dumpdtb={}", dtb_dump_path.display()));
            // machine = format!("{},dumpdtb=target/qemu.dtb", machine);
        }

        if need_machine {
            cmd.arg("-machine").arg(machine);
        }

        if self.tool.debug_enabled() {
            cmd.arg("-s").arg("-S");
        }

        let mut use_kernel_loader = true;
        if let Some(uefi) = self.prepare_uefi().await? {
            match uefi {
                UefiBootConfig::Pflash {
                    code,
                    vars,
                    esp_dir,
                } => {
                    cmd.arg("-drive").arg(format!(
                        "if=pflash,format=raw,unit=0,readonly=on,file={}",
                        code.display()
                    ));
                    cmd.arg("-drive").arg(format!(
                        "if=pflash,format=raw,unit=1,file={}",
                        vars.display()
                    ));
                    cmd.arg("-drive")
                        .arg(format!("format=raw,file=fat:rw:{}", esp_dir.display()));
                    use_kernel_loader = false;
                }
            }
        }

        if use_kernel_loader {
            if let Some(bin_path) = &self.tool.ctx.artifacts.bin {
                cmd.arg("-kernel").arg(bin_path);
            } else if let Some(elf_path) = &self.tool.ctx.artifacts.elf {
                cmd.arg("-kernel").arg(elf_path);
            }
        }
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.print_cmd();
        let mut child = TokioCommand::from(cmd.into_std()).spawn()?;
        let stdin = child.stdin.take().context("failed to capture QEMU stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("failed to capture QEMU stdout")?;
        let stderr = child
            .stderr
            .take()
            .context("failed to capture QEMU stderr")?;

        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let stderr_capture = Arc::new(Mutex::new(Vec::<u8>::new()));

        let stdout_task = tokio::spawn(read_child_stream(stdout, inbound_tx.clone(), None));
        let stderr_task = tokio::spawn(read_child_stream(
            stderr,
            inbound_tx,
            Some(stderr_capture.clone()),
        ));
        let write_task = tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(bytes) = outbound_rx.recv().await {
                if let Err(err) = stdin.write_all(&bytes).await {
                    if err.kind() != ErrorKind::BrokenPipe {
                        return Err(err).context("failed to forward stdin to QEMU");
                    }
                    break;
                }
                stdin.flush().await.context("failed to flush QEMU stdin")?;
            }
            Ok::<(), anyhow::Error>(())
        });

        let matcher = Arc::new(Mutex::new(ByteStreamMatcher::new(
            self.success_regex.clone(),
            self.fail_regex.clone(),
        )));
        let shell_auto_init = Arc::new(Mutex::new(self.config.shell_auto_init()));
        let match_result = Arc::new(Mutex::new(None::<anyhow::Result<()>>));
        let terminal = AsyncTerminal::new(TerminalConfig {
            intercept_exit_sequence: false,
            timeout: timeout_duration(self.config.timeout),
            timeout_label: "QEMU".to_string(),
        });

        let terminal_result = terminal
            .run(inbound_rx, outbound_tx, {
                let matcher = matcher.clone();
                let shell_auto_init = shell_auto_init.clone();
                let match_result = match_result.clone();
                move |handle, byte| {
                    let mut matcher = matcher.lock().unwrap();
                    if let Some(matched) = matcher.observe_byte(byte) {
                        print_match_event(&matched);
                        let mut result = match_result.lock().unwrap();
                        *result = Some(matched.kind.into_result(&matched));
                        handle.stop_after(crate::run::output_matcher::MATCH_DRAIN_DURATION);
                    }

                    let mut shell_auto_init = shell_auto_init.lock().unwrap();
                    if let Some(shell_auto_init) = shell_auto_init.as_mut()
                        && let Some(command) = shell_auto_init.observe_byte(byte)
                    {
                        handle.send_after(SHELL_INIT_DELAY, command);
                    }

                    if matcher.should_stop() {
                        handle.stop();
                    }
                }
            })
            .await;

        let should_kill = matcher.lock().unwrap().should_stop() || terminal_result.is_err();
        if should_kill
            && child
                .try_wait()
                .context("failed to query QEMU process status")?
                .is_none()
            && let Err(err) = child.kill().await
            && err.kind() != ErrorKind::InvalidInput
        {
            return Err(err.into());
        }

        let status = child.wait().await?;
        let _ = stdout_task.await;
        let _ = stderr_task.await;
        let _ = write_task.await;

        terminal_result?;

        if let Some(result) = match_result.lock().unwrap().take() {
            result?;
        } else if !status.success() {
            unsafe {
                return Err(anyhow::anyhow!(
                    "{}",
                    OsString::from_encoded_bytes_unchecked(stderr_capture.lock().unwrap().clone())
                        .to_string_lossy()
                ));
            }
        }
        Ok(())
    }

    async fn prepare_uefi(&self) -> anyhow::Result<Option<UefiBootConfig>> {
        if !self.config.uefi {
            return Ok(None);
        }

        let arch =
            self.tool.ctx.arch.as_ref().ok_or_else(|| {
                anyhow::anyhow!("Cannot determine architecture for OVMF preparation")
            })?;
        let tmp = std::env::temp_dir();
        let bios_dir = tmp.join("ostool").join("ovmf");
        fs::create_dir_all(&bios_dir)
            .await
            .with_path("failed to create directory", &bios_dir)?;

        println!("Preparing OVMF firmware for architecture: {:?}", arch);
        let prebuilt = Prebuilt::fetch(Source::LATEST, &bios_dir)
            .with_context(|| format!("failed to prepare OVMF cache: {}", bios_dir.display()))?;
        let arch = match arch {
            Architecture::X86_64 => Arch::X64,
            Architecture::Aarch64 => Arch::Aarch64,
            Architecture::Riscv64 => Arch::Riscv64,
            Architecture::LoongArch64 => Arch::LoongArch64,
            Architecture::I386 => Arch::Ia32,
            o => return Err(anyhow::anyhow!("OVMF is not supported for {o:?} ",)),
        };

        let code = prebuilt.get_file(arch, FileType::Code);
        let vars_template = prebuilt.get_file(arch, FileType::Vars);
        let esp_dir = self.prepare_uefi_esp(arch).await?;
        let vars = self.prepare_uefi_vars(&vars_template).await?;

        Ok(Some(UefiBootConfig::Pflash {
            code,
            vars,
            esp_dir,
        }))
    }

    async fn prepare_uefi_esp(&self, arch: Arch) -> anyhow::Result<PathBuf> {
        let bin_path = self
            .tool
            .ctx
            .artifacts
            .bin
            .as_ref()
            .ok_or_else(|| anyhow!("UEFI boot requires a BIN artifact"))?;
        let stem = bin_path
            .file_stem()
            .ok_or_else(|| anyhow!("invalid BIN path: {}", bin_path.display()))?;
        let artifact_dir = self.uefi_artifact_dir(bin_path)?;
        let esp_dir = artifact_dir.join(format!("{}.esp", stem.to_string_lossy()));
        let boot_dir = esp_dir.join("EFI").join("BOOT");
        fs::create_dir_all(&boot_dir)
            .await
            .with_path("failed to create directory", &boot_dir)?;

        let boot_path = boot_dir.join(Self::default_uefi_boot_filename(arch));
        fs::copy(bin_path, &boot_path).await.with_context(|| {
            format!(
                "failed to copy EFI image from {} to {}",
                bin_path.display(),
                boot_path.display()
            )
        })?;

        Ok(esp_dir)
    }

    fn uefi_artifact_dir(&self, bin_path: &Path) -> anyhow::Result<PathBuf> {
        if let Some(dir) = &self.tool.ctx.artifacts.runtime_artifact_dir {
            return Ok(dir.clone());
        }

        let bin_path = bin_path
            .canonicalize()
            .with_path("failed to canonicalize file", bin_path)?;
        bin_path
            .parent()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("invalid BIN path: {}", bin_path.display()))
    }

    async fn prepare_uefi_vars(&self, vars_template: &Path) -> anyhow::Result<PathBuf> {
        let bin_path = self
            .tool
            .ctx
            .artifacts
            .bin
            .as_ref()
            .ok_or_else(|| anyhow!("UEFI boot requires a BIN artifact"))?;
        let stem = bin_path
            .file_stem()
            .ok_or_else(|| anyhow!("invalid BIN path: {}", bin_path.display()))?;
        let artifact_dir = self.uefi_artifact_dir(bin_path)?;
        fs::create_dir_all(&artifact_dir)
            .await
            .with_path("failed to create directory", &artifact_dir)?;

        let vars = artifact_dir.join(format!("{}.vars.fd", stem.to_string_lossy()));
        fs::copy(vars_template, &vars).await.with_context(|| {
            format!(
                "failed to copy OVMF vars from {} to {}",
                vars_template.display(),
                vars.display()
            )
        })?;

        Ok(vars)
    }

    fn default_uefi_boot_filename(arch: Arch) -> &'static str {
        match arch {
            Arch::Aarch64 => "BOOTAA64.EFI",
            Arch::Ia32 => "BOOTIA32.EFI",
            Arch::LoongArch64 => "BOOTLOONGARCH64.EFI",
            Arch::Riscv64 => "BOOTRISCV64.EFI",
            Arch::X64 => "BOOTX64.EFI",
        }
    }

    fn prepare_regex(&mut self) -> anyhow::Result<()> {
        let (success, fail) = compile_regexes(&self.config.success_regex, &self.config.fail_regex)?;
        self.success_regex = success;
        self.fail_regex = fail;
        Ok(())
    }
}

/// Resolve QEMU configuration file path with architecture-specific priority.
///
/// Configuration search priority:
/// 1. Explicit path (if provided)
/// 2. workspace_dir: qemu-<arch>.toml → .qemu-<arch>.toml → qemu.toml → .qemu.toml
///
/// When architecture is detected, architecture-specific files are checked first.
pub(crate) fn resolve_qemu_config_path(
    tool: &Tool,
    explicit_path: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    resolve_qemu_config_path_in_dir(tool.workspace_dir(), tool.ctx.arch, explicit_path)
}

pub(crate) fn resolve_qemu_config_path_in_dir(
    search_dir: &Path,
    arch: Option<Architecture>,
    explicit_path: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit_path {
        return Ok(path);
    }

    let arch_str = arch.map(|arch| format!("{arch:?}").to_lowercase());

    // 文件名优先级顺序
    let candidates: Vec<String> = if let Some(ref arch) = arch_str {
        vec![
            format!("qemu-{}.toml", arch),
            format!(".qemu-{}.toml", arch),
            "qemu.toml".to_string(),
            ".qemu.toml".to_string(),
        ]
    } else {
        vec!["qemu.toml".to_string(), ".qemu.toml".to_string()]
    };

    for filename in &candidates {
        let path = search_dir.join(filename);
        if path.exists() {
            return Ok(path);
        }
    }

    let default_filename = if let Some(ref arch) = arch_str {
        format!(".qemu-{}.toml", arch)
    } else {
        ".qemu.toml".to_string()
    };

    Ok(search_dir.join(default_filename))
}

fn timeout_duration(timeout: Option<u64>) -> Option<Duration> {
    match timeout {
        Some(0) | None => None,
        Some(secs) => Some(Duration::from_secs(secs)),
    }
}

async fn read_child_stream<R>(
    mut reader: R,
    tx: mpsc::UnboundedSender<Vec<u8>>,
    capture: Option<Arc<Mutex<Vec<u8>>>>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
{
    // Use a 64 KiB read buffer so that a full ratatui TUI frame (typically
    // 8–32 KiB of ANSI escape sequences) arrives in one chunk rather than in
    // many 1 KiB pieces.  The sterm run_loop drains all available chunks with
    // try_recv before flushing to the host terminal, so larger reads mean fewer
    // intermediate renders and no visible line-by-line TUI flicker.
    let mut buffer = vec![0u8; 65536];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        if let Some(capture) = capture.as_ref() {
            capture.lock().unwrap().extend_from_slice(&buffer[..read]);
        }
        if tx.send(buffer[..read].to_vec()).is_err() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        QemuConfig, QemuRunner, build_default_qemu_config, ensure_qemu_config_at_path,
        infer_target_arch, read_qemu_config_at_path, resolve_qemu_config_path,
        resolve_qemu_config_path_in_dir, timeout_duration,
    };
    use object::Architecture;
    use std::{path::PathBuf, time::Duration};
    use tempfile::TempDir;

    use crate::{
        Tool, ToolConfig,
        build::config::{BuildConfig, BuildSystem, Cargo},
        run::{
            output_matcher::{ByteStreamMatcher, StreamMatchKind},
            shell_init::ShellAutoInitMatcher,
        },
    };
    use std::collections::HashMap;

    fn write_single_crate_manifest(dir: &std::path::Path) {
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"sample\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "").unwrap();
    }

    fn make_tool(dir: &std::path::Path) -> Tool {
        Tool::new(ToolConfig {
            manifest: Some(dir.to_path_buf()),
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn default_qemu_config_keeps_existing_defaults_without_overrides() {
        let config = build_default_qemu_config(Some(Architecture::Aarch64));

        assert!(config.to_bin);
        assert_eq!(config.args, vec!["-nographic", "-cpu", "cortex-a53"]);
        assert!(config.success_regex.is_empty());
        assert!(config.fail_regex.is_empty());
        assert_eq!(config.timeout, None);
    }

    #[test]
    fn default_qemu_config_for_other_arch_only_adds_generic_defaults() {
        let config = build_default_qemu_config(Some(Architecture::X86_64));

        assert!(config.to_bin);
        assert_eq!(config.args, vec!["-nographic"]);
        assert_eq!(config.timeout, None);
    }

    #[test]
    fn infer_target_arch_maps_known_target_triples() {
        assert_eq!(
            infer_target_arch("aarch64-unknown-none"),
            Some(Architecture::Aarch64)
        );
        assert_eq!(
            infer_target_arch("riscv64gc-unknown-none-elf"),
            Some(Architecture::Riscv64)
        );
        assert_eq!(
            infer_target_arch("x86_64-unknown-none"),
            Some(Architecture::X86_64)
        );
        assert_eq!(infer_target_arch(""), None);
    }

    #[tokio::test]
    async fn load_existing_qemu_config_preserves_file_contents() {
        let tmp = TempDir::new().unwrap();
        write_single_crate_manifest(tmp.path());
        let config_path = tmp.path().join(".qemu.toml");
        std::fs::write(
            &config_path,
            r#"
args = ["-nographic", "-machine", "virt"]
uefi = false
to_bin = false
success_regex = ["PASS"]
fail_regex = ["FAIL"]
shell_prefix = "login:"
shell_init_cmd = "root"
"#,
        )
        .unwrap();

        let mut tool = make_tool(tmp.path());
        tool.ctx.arch = Some(Architecture::Aarch64);

        let config = read_qemu_config_at_path(&tool, config_path).await.unwrap();

        assert!(!config.to_bin);
        assert_eq!(config.success_regex, vec!["PASS"]);
        assert_eq!(config.fail_regex, vec!["FAIL"]);
        assert_eq!(config.shell_prefix.as_deref(), Some("login:"));
        assert_eq!(config.shell_init_cmd.as_deref(), Some("root"));
        assert_eq!(config.args, vec!["-nographic", "-machine", "virt"]);
    }

    #[tokio::test]
    async fn load_missing_qemu_config_uses_default_template() {
        let tmp = TempDir::new().unwrap();
        write_single_crate_manifest(tmp.path());
        let config_path = tmp.path().join(".qemu.toml");

        let mut tool = make_tool(tmp.path());
        tool.ctx.arch = Some(Architecture::Aarch64);

        let config = ensure_qemu_config_at_path(
            &tool,
            config_path.clone(),
            build_default_qemu_config(Some(Architecture::Aarch64)),
        )
        .await
        .unwrap();

        assert!(config.to_bin);
        assert_eq!(config.args, vec!["-nographic", "-cpu", "cortex-a53"]);
        assert!(config_path.exists());
    }

    #[tokio::test]
    async fn load_qemu_config_for_cargo_prefers_package_dir() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\", \"kernel\"]\nresolver = \"3\"\n",
        )
        .unwrap();

        let app_dir = tmp.path().join("app");
        std::fs::create_dir_all(app_dir.join("src")).unwrap();
        std::fs::write(
            app_dir.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(app_dir.join("src/main.rs"), "fn main() {}\n").unwrap();

        let kernel_dir = tmp.path().join("kernel");
        std::fs::create_dir_all(kernel_dir.join("src")).unwrap();
        std::fs::write(
            kernel_dir.join("Cargo.toml"),
            "[package]\nname = \"kernel\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(kernel_dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(
            kernel_dir.join(".qemu-aarch64.toml"),
            r#"
args = ["-custom"]
uefi = false
to_bin = true
success_regex = []
fail_regex = []
"#,
        )
        .unwrap();

        let mut tool = Tool::new(ToolConfig {
            manifest: Some(app_dir),
            ..Default::default()
        })
        .unwrap();

        let config = tool
            .ensure_qemu_config_for_cargo(&Cargo {
                env: HashMap::new(),
                target: "aarch64-unknown-none".into(),
                package: "kernel".into(),
                features: vec![],
                log: None,
                extra_config: None,
                args: vec![],
                pre_build_cmds: vec![],
                post_build_cmds: vec![],
                to_bin: false,
            })
            .await
            .unwrap();

        assert_eq!(config.args, vec!["-custom"]);
    }

    #[test]
    fn default_qemu_config_for_cargo_uses_target_arch() {
        let tool = Tool::new(ToolConfig::default()).unwrap();
        let config = tool.default_qemu_config_for_cargo(&Cargo {
            env: HashMap::new(),
            target: "riscv64gc-unknown-none-elf".into(),
            package: "sample".into(),
            features: vec![],
            log: None,
            extra_config: None,
            args: vec![],
            pre_build_cmds: vec![],
            post_build_cmds: vec![],
            to_bin: false,
        });

        assert_eq!(config.args, vec!["-nographic", "-cpu", "rv64"]);
    }

    #[test]
    fn qemu_timeout_zero_disables_timeout() {
        assert_eq!(timeout_duration(None), None);
        assert_eq!(timeout_duration(Some(0)), None);
        assert_eq!(timeout_duration(Some(3)), Some(Duration::from_secs(3)));
    }

    #[test]
    fn qemu_config_parses_timeout_from_toml() {
        let config: QemuConfig = toml::from_str(
            r#"
args = ["-nographic"]
uefi = false
to_bin = true
success_regex = []
fail_regex = []
timeout = 0
"#,
        )
        .unwrap();

        assert_eq!(config.timeout, Some(0));
    }

    #[test]
    fn qemu_config_normalize_rejects_shell_init_without_prefix() {
        let mut config = QemuConfig {
            shell_init_cmd: Some("root".into()),
            ..Default::default()
        };

        let err = config.normalize("test config").unwrap_err();
        assert!(err.to_string().contains("shell_prefix"));
    }

    #[test]
    fn qemu_config_normalize_trims_shell_fields() {
        let mut config = QemuConfig {
            shell_prefix: Some(" login: ".into()),
            shell_init_cmd: Some(" root ".into()),
            ..Default::default()
        };

        config.normalize("test config").unwrap();

        assert_eq!(config.shell_prefix.as_deref(), Some("login:"));
        assert_eq!(config.shell_init_cmd.as_deref(), Some("root"));
    }

    #[test]
    fn qemu_shell_auto_init_can_coexist_with_success_matcher() {
        let mut matcher = ByteStreamMatcher::new(
            vec![regex::Regex::new("ready").unwrap()],
            vec![regex::Regex::new("__never_fail__").unwrap()],
        );
        let mut shell_init =
            ShellAutoInitMatcher::new(Some("login:".to_string()), Some("root".to_string()))
                .unwrap();
        let mut sent = None;

        for byte in b"login: system ready\n" {
            if sent.is_none() {
                sent = shell_init.observe_byte(*byte);
            } else {
                let _ = shell_init.observe_byte(*byte);
            }
            let _ = matcher.observe_byte(*byte);
        }

        let matched = matcher.matched().unwrap();
        assert_eq!(matched.kind, StreamMatchKind::Success);
        assert_eq!(sent.as_deref(), Some(&b"root\n"[..]));
    }

    #[test]
    fn uefi_artifact_dir_prefers_runtime_artifact_dir() {
        let runtime_dir = PathBuf::from("/tmp/ostool-runtime");
        let tmp = TempDir::new().unwrap();
        write_single_crate_manifest(tmp.path());
        let mut tool = make_tool(tmp.path());
        tool.ctx.artifacts.runtime_artifact_dir = Some(runtime_dir.clone());

        let runner = QemuRunner {
            tool: &mut tool,
            config: QemuConfig::default(),
            dtbdump: false,
            success_regex: vec![],
            fail_regex: vec![],
        };

        let resolved = runner
            .uefi_artifact_dir(PathBuf::from("/tmp/ignored/kernel.bin").as_path())
            .unwrap();
        assert_eq!(resolved, runtime_dir);
    }

    // === QEMU 配置路径解析测试 ===

    #[test]
    fn qemu_config_explicit_path_wins() {
        let tmp = TempDir::new().unwrap();
        write_single_crate_manifest(tmp.path());
        let tool = make_tool(tmp.path());

        let explicit = tmp.path().join("custom.qemu.toml");
        let result = resolve_qemu_config_path(&tool, Some(explicit.clone())).unwrap();
        assert_eq!(result, explicit);
    }

    #[test]
    fn qemu_config_workspace_path_used() {
        let tmp = TempDir::new().unwrap();
        write_single_crate_manifest(tmp.path());
        std::fs::write(tmp.path().join("qemu-aarch64.toml"), "").unwrap();

        let mut tool = make_tool(tmp.path());
        tool.ctx.arch = Some(Architecture::Aarch64);

        let result = resolve_qemu_config_path(&tool, None).unwrap();
        assert_eq!(result, tmp.path().join("qemu-aarch64.toml"));
    }

    #[test]
    fn qemu_config_filename_priority() {
        let tmp = TempDir::new().unwrap();
        write_single_crate_manifest(tmp.path());
        let manifest = tmp.path().to_path_buf();
        let mut tool = make_tool(tmp.path());
        tool.ctx.arch = Some(Architecture::Aarch64);

        std::fs::write(manifest.join("qemu.toml"), "").unwrap();
        let result = resolve_qemu_config_path(&tool, None).unwrap();
        assert_eq!(result, manifest.join("qemu.toml"));

        std::fs::write(manifest.join("qemu-aarch64.toml"), "").unwrap();
        let result = resolve_qemu_config_path(&tool, None).unwrap();
        assert_eq!(result, manifest.join("qemu-aarch64.toml"));
    }

    #[test]
    fn qemu_config_replaces_string_fields() {
        let tmp = TempDir::new().unwrap();
        write_single_crate_manifest(tmp.path());
        let mut tool = make_tool(tmp.path());
        tool.ctx.build_config = Some(BuildConfig {
            system: BuildSystem::Cargo(Cargo {
                env: HashMap::new(),
                target: "aarch64-unknown-none".into(),
                package: "sample".into(),
                features: vec![],
                log: None,
                extra_config: None,
                args: vec![],
                pre_build_cmds: vec![],
                post_build_cmds: vec![],
                to_bin: false,
            }),
        });
        unsafe {
            std::env::set_var("OSTOOL_QEMU_TEST_ENV", "env-ok");
        }

        let mut config = QemuConfig {
            args: vec!["${workspace}".into(), "${package}".into()],
            success_regex: vec!["${env:OSTOOL_QEMU_TEST_ENV}".into()],
            fail_regex: vec!["${workspaceFolder}".into()],
            shell_prefix: Some("${workspace}".into()),
            shell_init_cmd: Some("${package}".into()),
            ..Default::default()
        };

        config.replace_strings(&tool).unwrap();

        let expected = tmp.path().display().to_string();
        assert_eq!(config.args, vec![expected.clone(), expected.clone()]);
        assert_eq!(config.success_regex, vec!["env-ok"]);
        assert_eq!(config.fail_regex, vec![expected.clone()]);
        assert_eq!(config.shell_prefix.as_deref(), Some(expected.as_str()));
        assert_eq!(config.shell_init_cmd.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn qemu_config_explicit_path_supports_variables() {
        let tmp = TempDir::new().unwrap();
        write_single_crate_manifest(tmp.path());
        let tool = make_tool(tmp.path());

        let result = resolve_qemu_config_path(
            &tool,
            Some(
                tool.replace_path_variables("${workspace}/qemu.toml".into())
                    .unwrap(),
            ),
        )
        .unwrap();
        assert_eq!(result, tmp.path().join("qemu.toml"));
    }

    #[test]
    fn qemu_config_default_path_with_search_dir() {
        let tmp = TempDir::new().unwrap();
        write_single_crate_manifest(tmp.path());
        let tool = make_tool(tmp.path());

        let result = resolve_qemu_config_path(&tool, None).unwrap();
        assert_eq!(result, tmp.path().join(".qemu.toml"));
    }

    #[test]
    fn qemu_config_default_path_with_arch() {
        let tmp = TempDir::new().unwrap();
        write_single_crate_manifest(tmp.path());
        let mut tool = make_tool(tmp.path());
        tool.ctx.arch = Some(Architecture::Aarch64);

        let result = resolve_qemu_config_path(&tool, None).unwrap();
        assert_eq!(result, tmp.path().join(".qemu-aarch64.toml"));
    }

    #[test]
    fn qemu_config_without_arch() {
        let tmp = TempDir::new().unwrap();
        write_single_crate_manifest(tmp.path());
        std::fs::write(tmp.path().join("qemu-aarch64.toml"), "").unwrap();
        std::fs::write(tmp.path().join("qemu.toml"), "").unwrap();

        let tool = make_tool(tmp.path());
        let result = resolve_qemu_config_path(&tool, None).unwrap();
        assert_eq!(result, tmp.path().join("qemu.toml"));
    }

    #[test]
    fn qemu_config_search_dir_prefers_arch_specific_files() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("qemu-aarch64.toml"), "").unwrap();
        std::fs::write(tmp.path().join("qemu.toml"), "").unwrap();

        let result =
            resolve_qemu_config_path_in_dir(tmp.path(), Some(Architecture::Aarch64), None).unwrap();
        assert_eq!(result, tmp.path().join("qemu-aarch64.toml"));
    }

    #[test]
    fn qemu_config_search_dir_uses_hidden_generic_before_hidden_default_creation() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".qemu.toml"), "").unwrap();

        let result =
            resolve_qemu_config_path_in_dir(tmp.path(), Some(Architecture::Aarch64), None).unwrap();
        assert_eq!(result, tmp.path().join(".qemu.toml"));
    }

    #[test]
    fn qemu_config_search_dir_defaults_to_arch_specific_hidden_file() {
        let tmp = TempDir::new().unwrap();

        let result =
            resolve_qemu_config_path_in_dir(tmp.path(), Some(Architecture::Aarch64), None).unwrap();
        assert_eq!(result, tmp.path().join(".qemu-aarch64.toml"));
    }

    #[test]
    fn qemu_config_search_dir_defaults_without_arch() {
        let tmp = TempDir::new().unwrap();

        let result = resolve_qemu_config_path_in_dir(tmp.path(), None, None).unwrap();
        assert_eq!(result, tmp.path().join(".qemu.toml"));
    }

    #[test]
    fn build_config_explicit_path_wins() {
        let tmp = TempDir::new().unwrap();
        write_single_crate_manifest(tmp.path());
        let tool = make_tool(tmp.path());

        let explicit = tmp.path().join("custom.build.toml");
        let result = tool.resolve_build_config_path(Some(explicit.clone()));
        assert_eq!(result, explicit);
    }

    #[test]
    fn build_config_defaults_to_workspace_root() {
        let tmp = TempDir::new().unwrap();
        write_single_crate_manifest(tmp.path());
        let tool = make_tool(tmp.path());

        let result = tool.resolve_build_config_path(None);
        assert_eq!(result, tmp.path().join(".build.toml"));
    }
}
