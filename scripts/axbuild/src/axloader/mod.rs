use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command as StdCommand, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use ostool::ovmf::Arch;

use crate::support::{ovmf::OvmfFirmware, process::ProcessExt};

const AXLOADER_PACKAGE: &str = "axloader";
const AXLOADER_BIN: &str = "axloader";
const DEFAULT_UEFI_TARGET: &str = "x86_64-unknown-uefi";
const HTTP_SMOKE_BOOT_TIMEOUT: Duration = Duration::from_secs(240);
const HTTP_SMOKE_TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_SMOKE_MAX_ATTEMPTS: usize = 2;
const QEMU_HOST_GATEWAY: &str = "10.0.2.2";

#[derive(Clone, Copy)]
struct LoaderSmokeTarget {
    arch: &'static str,
    ovmf_arch: Arch,
    efi_output_file: &'static str,
    qemu_program: &'static str,
    qemu_args: fn(&Path, &Path) -> Vec<String>,
    kernel_elf: fn() -> Vec<u8>,
}

struct SmokeAttemptContext<'a> {
    workspace_root: &'a Path,
    target: &'a str,
    smoke_target: LoaderSmokeTarget,
    firmware: &'a Path,
    kernel: &'a [u8],
}

struct SmokeAttemptProgress {
    deadline: Instant,
    boot_sent: bool,
}

impl SmokeAttemptProgress {
    fn waiting_for_ready(started: Instant) -> Self {
        Self {
            deadline: started + HTTP_SMOKE_BOOT_TIMEOUT,
            boot_sent: false,
        }
    }

    fn mark_boot_sent(&mut self, sent_at: Instant) {
        self.boot_sent = true;
        self.deadline = sent_at + HTTP_SMOKE_TRANSFER_TIMEOUT;
    }

    fn boot_sent(&self) -> bool {
        self.boot_sent
    }

    fn expired_at(&self, now: Instant) -> bool {
        now >= self.deadline
    }
}

#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct ArgsBuild {
    #[arg(long, default_value = DEFAULT_UEFI_TARGET)]
    pub target: String,

    #[arg(long, conflicts_with = "debug")]
    pub release: bool,

    #[arg(long, conflicts_with = "release")]
    pub debug: bool,
}

#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct ArgsTest {
    #[command(subcommand)]
    pub command: TestCommand,
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum TestCommand {
    /// Run axloader host checks and QEMU HTTP smoke test
    Qemu(ArgsTestQemu),
}

#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct ArgsTestQemu {
    #[arg(long, default_value = DEFAULT_UEFI_TARGET)]
    pub target: String,
}

/// Axloader host-side commands
#[derive(Subcommand)]
pub enum Command {
    /// Build axloader
    Build(ArgsBuild),
    /// Run axloader test suites
    Test(ArgsTest),
}

pub struct Axloader {
    workspace_root: PathBuf,
}

impl Axloader {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            workspace_root: crate::context::workspace_root_path()?,
        })
    }

    pub async fn execute(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Build(args) => build(&self.workspace_root, args),
            Command::Test(args) => test(&self.workspace_root, args).await,
        }
    }
}

pub fn build(workspace_root: &Path, args: ArgsBuild) -> anyhow::Result<()> {
    run_loader_build(workspace_root, &args.target, args.release || !args.debug)
}

pub async fn test(workspace_root: &Path, args: ArgsTest) -> anyhow::Result<()> {
    match args.command {
        TestCommand::Qemu(args) => test_qemu(workspace_root, args).await,
    }
}

async fn test_qemu(workspace_root: &Path, args: ArgsTestQemu) -> anyhow::Result<()> {
    run_cargo(
        workspace_root,
        ["test", "-p", AXLOADER_PACKAGE, "--all-targets"],
    )?;
    let result = run_cargo(
        workspace_root,
        [
            "check",
            "-p",
            AXLOADER_PACKAGE,
            "--target",
            args.target.as_str(),
            "--bin",
            AXLOADER_BIN,
        ],
    );
    result?;

    run_http_smoke_test(workspace_root, &args.target).await
}

fn run_loader_build(workspace_root: &Path, target: &str, release: bool) -> anyhow::Result<()> {
    let mut args = vec![
        "build",
        "-p",
        AXLOADER_PACKAGE,
        "--target",
        target,
        "--bin",
        AXLOADER_BIN,
    ];
    if release {
        args.push("--release");
    }
    run_cargo(workspace_root, args)
}

fn run_cargo<'a>(
    workspace_root: &Path,
    args: impl IntoIterator<Item = &'a str>,
) -> anyhow::Result<()> {
    let mut command = StdCommand::new("cargo");
    command.current_dir(workspace_root).args(args);
    command.exec()
}

async fn run_http_smoke_test(workspace_root: &Path, target: &str) -> anyhow::Result<()> {
    let smoke_target = smoke_target(target)?;

    println!("axloader http smoke: building UEFI loader ...");
    run_loader_build(workspace_root, target, true)?;

    let firmware = OvmfFirmware::fetch(smoke_target.ovmf_arch).await?;
    println!(
        "axloader http smoke: using UEFI firmware {}",
        firmware.code().display()
    );
    let kernel = (smoke_target.kernel_elf)();
    let attempt_context = SmokeAttemptContext {
        workspace_root,
        target,
        smoke_target,
        firmware: firmware.code(),
        kernel: &kernel,
    };
    let mut attempt = 1;
    let mut failures = Vec::new();

    loop {
        println!(
            "axloader http smoke: running QEMU attempt {attempt}/{HTTP_SMOKE_MAX_ATTEMPTS} ..."
        );
        let failure = match run_http_smoke_attempt(&attempt_context) {
            Ok(()) => {
                println!("axloader http smoke: kernel transferred and ELF loaded");
                return Ok(());
            }
            Err(error) => format!("attempt {attempt}: {error:#}"),
        };

        let Some(next_attempt) = next_smoke_attempt(attempt) else {
            failures.push(failure);
            bail!(
                "axloader HTTP smoke failed after {attempt} attempt(s):\n{}",
                failures.join("\n")
            );
        };
        eprintln!("axloader http smoke: {failure}; retrying with a fresh QEMU instance");
        failures.push(failure);
        attempt = next_attempt;
    }
}

fn run_http_smoke_attempt(context: &SmokeAttemptContext<'_>) -> anyhow::Result<()> {
    let temp = tempfile::tempdir().context("failed to create axloader HTTP smoke temp dir")?;
    let efi_boot_dir = temp.path().join("esp/EFI/BOOT");
    fs::create_dir_all(&efi_boot_dir)
        .with_context(|| format!("failed to create {}", efi_boot_dir.display()))?;
    fs::copy(
        axloader_efi_path(context.workspace_root, context.target),
        efi_boot_dir.join(context.smoke_target.efi_output_file),
    )
    .context("failed to stage axloader EFI binary")?;

    let http_server = SmokeHttpServer::start(context.kernel.to_vec())?;
    let boot_line = format_boot_line(
        context.smoke_target.arch,
        context.kernel.len(),
        http_server.port(),
    );

    let mut child = spawn_axloader_qemu(
        context.smoke_target,
        context.firmware,
        &temp.path().join("esp"),
    )?;
    let smoke_result = drive_http_smoke_session(&mut child, &boot_line);
    stop_child(&mut child);
    smoke_result?;

    if !http_server.was_requested() {
        bail!("axloader HTTP smoke reached elf_loaded without observing /kernel.elf request");
    }

    Ok(())
}

fn drive_http_smoke_session(child: &mut Child, boot_line: &str) -> anyhow::Result<()> {
    let mut stdin = child
        .stdin
        .take()
        .context("failed to capture QEMU stdin for serial control")?;
    let stdout = child
        .stdout
        .take()
        .context("failed to capture QEMU stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture QEMU stderr")?;
    let (output_tx, output_rx) = mpsc::channel();
    spawn_output_reader(stdout, output_tx.clone());
    spawn_output_reader(stderr, output_tx);

    let mut progress = SmokeAttemptProgress::waiting_for_ready(Instant::now());
    let mut transcript = String::new();
    while !progress.expired_at(Instant::now()) {
        match output_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => {
                print!("{chunk}");
                transcript.push_str(&chunk);
                if !progress.boot_sent() && transcript.contains("AXLOADER READY") {
                    stdin
                        .write_all(boot_line.as_bytes())
                        .context("failed to send AXLOADER BOOT over QEMU serial")?;
                    stdin.flush().ok();
                    progress.mark_boot_sent(Instant::now());
                }
                if transcript.contains("elf_loaded:") {
                    return Ok(());
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(status) = child.try_wait()? {
                    bail!(
                        "QEMU exited before elf_loaded with status {status}; \
                         transcript:\n{transcript}"
                    );
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let status = child
                    .try_wait()?
                    .map_or_else(|| "unknown".to_owned(), |status| status.to_string());
                bail!(
                    "QEMU output closed before elf_loaded with status {status}; \
                     transcript:\n{transcript}"
                );
            }
        }
    }

    let phase = if progress.boot_sent() {
        "kernel transfer"
    } else {
        "UEFI startup"
    };
    bail!("axloader HTTP smoke timed out during {phase}; transcript:\n{transcript}")
}

fn next_smoke_attempt(current_attempt: usize) -> Option<usize> {
    (current_attempt < HTTP_SMOKE_MAX_ATTEMPTS).then_some(current_attempt + 1)
}

fn format_boot_line(arch: &str, kernel_size: usize, http_port: u16) -> String {
    format!(
        concat!(
            "AXLOADER BOOT {{",
            "\"protocol_version\":1,",
            "\"boot_id\":\"ci-http-smoke\",",
            "\"kernel_url\":\"http://{}:{}/kernel.elf\",",
            "\"kernel_size\":{},",
            "\"image_format\":\"elf64\",",
            "\"arch\":\"{}\",",
            "\"entry_symbol\":null",
            "}}\n"
        ),
        QEMU_HOST_GATEWAY, http_port, kernel_size, arch,
    )
}

fn axloader_efi_path(workspace_root: &Path, target: &str) -> PathBuf {
    workspace_root
        .join("target")
        .join(target)
        .join("release")
        .join("axloader.efi")
}

fn smoke_target(target: &str) -> anyhow::Result<LoaderSmokeTarget> {
    match target {
        "x86_64-unknown-uefi" => Ok(LoaderSmokeTarget {
            arch: "x86_64",
            ovmf_arch: Arch::X64,
            efi_output_file: "BOOTX64.EFI",
            qemu_program: "qemu-system-x86_64",
            qemu_args: x86_64_qemu_args,
            kernel_elf: minimal_x86_64_kernel_elf,
        }),
        _ => bail!("axloader HTTP smoke does not support target `{target}`"),
    }
}

fn spawn_axloader_qemu(
    target: LoaderSmokeTarget,
    firmware: &Path,
    esp_dir: &Path,
) -> anyhow::Result<Child> {
    StdCommand::new(target.qemu_program)
        .args((target.qemu_args)(firmware, esp_dir))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "failed to start {} for axloader HTTP smoke",
                target.qemu_program
            )
        })
}

fn x86_64_qemu_args(firmware: &Path, esp_dir: &Path) -> Vec<String> {
    [
        "-m".into(),
        "256M".into(),
        "-smp".into(),
        "1".into(),
        "-machine".into(),
        "q35".into(),
        "-accel".into(),
        "kvm".into(),
        "-cpu".into(),
        // The pinned OVMF build does not publish its network protocols with
        // QEMU's restricted default CPU. This smoke runs on KVM-labelled hosts.
        "host".into(),
        "-display".into(),
        "none".into(),
        "-monitor".into(),
        "none".into(),
        "-serial".into(),
        "stdio".into(),
        "-netdev".into(),
        "user,id=net0".into(),
        "-device".into(),
        // ostool's OVMF prebuilt always includes VirtioNetDxe, while its E1000
        // driver is optional and absent from the pinned firmware build.
        "virtio-net-pci,netdev=net0".into(),
        "-drive".into(),
        format!(
            "if=pflash,format=raw,readonly=on,file={}",
            firmware.display()
        ),
        "-drive".into(),
        format!("format=raw,if=ide,file=fat:rw:{}", esp_dir.display()),
    ]
    .into()
}

fn spawn_output_reader(mut output: impl Read + Send + 'static, tx: mpsc::Sender<String>) {
    thread::spawn(move || {
        let mut byte = [0u8; 1];
        loop {
            match output.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    let _ = tx.send(String::from_utf8_lossy(&byte).into_owned());
                }
                Err(_) => break,
            }
        }
    });
}

fn stop_child(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    let _ = child.kill();
    let _ = child.wait();
}

struct SmokeHttpServer {
    stop: Arc<AtomicBool>,
    requested: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    port: u16,
}

impl SmokeHttpServer {
    fn start(body: Vec<u8>) -> anyhow::Result<Self> {
        let listener =
            TcpListener::bind("0.0.0.0:0").context("failed to bind axloader HTTP smoke server")?;
        let port = listener
            .local_addr()
            .context("failed to read axloader HTTP smoke server address")?
            .port();
        listener
            .set_nonblocking(true)
            .context("failed to configure axloader HTTP smoke server")?;

        let stop = Arc::new(AtomicBool::new(false));
        let requested = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread_requested = requested.clone();
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0u8; 1024];
                        let read = stream.read(&mut request).unwrap_or(0);
                        let request = String::from_utf8_lossy(&request[..read]);
                        if request.starts_with("GET /kernel.elf ") {
                            thread_requested.store(true, Ordering::Release);
                        }
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        if stream.write_all(header.as_bytes()).is_ok() {
                            let _ = stream.write_all(&body);
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        println!("axloader http smoke: serving kernel on 0.0.0.0:{port}");
        Ok(Self {
            stop,
            requested,
            thread: Some(thread),
            port,
        })
    }

    fn port(&self) -> u16 {
        self.port
    }

    fn was_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}

impl Drop for SmokeHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn minimal_x86_64_kernel_elf() -> Vec<u8> {
    const EHDR_SIZE: usize = 64;
    const PHDR_SIZE: usize = 56;
    const LOAD_OFFSET: usize = 0x1000;
    const LOAD_ADDR: u64 = 0x20_0000;
    const LOAD_MEM_SIZE: u64 = 0x1000;
    let code = [0xeb, 0xfe]; // jmp .
    let mut image = vec![0; LOAD_OFFSET + code.len()];

    image[0..4].copy_from_slice(b"\x7fELF");
    image[4] = 2;
    image[5] = 1;
    image[6] = 1;
    put_u16(&mut image, 16, 2);
    put_u16(&mut image, 18, 62);
    put_u32(&mut image, 20, 1);
    put_u64(&mut image, 24, LOAD_ADDR);
    put_u64(&mut image, 32, EHDR_SIZE as u64);
    put_u16(&mut image, 52, EHDR_SIZE as u16);
    put_u16(&mut image, 54, PHDR_SIZE as u16);
    put_u16(&mut image, 56, 1);

    let ph = EHDR_SIZE;
    put_u32(&mut image, ph, 1);
    put_u32(&mut image, ph + 4, 5);
    put_u64(&mut image, ph + 8, LOAD_OFFSET as u64);
    put_u64(&mut image, ph + 16, LOAD_ADDR);
    put_u64(&mut image, ph + 24, LOAD_ADDR);
    put_u64(&mut image, ph + 32, code.len() as u64);
    put_u64(&mut image, ph + 40, LOAD_MEM_SIZE);
    put_u64(&mut image, ph + 48, 0x1000);

    image[LOAD_OFFSET..LOAD_OFFSET + code.len()].copy_from_slice(&code);
    image
}

fn put_u16(image: &mut [u8], offset: usize, value: u16) {
    image[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(image: &mut [u8], offset: usize, value: u32) {
    image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(image: &mut [u8], offset: usize, value: u64) {
    image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        command: Command,
    }

    #[test]
    fn command_parses_build_default_target() {
        let cli = Cli::try_parse_from(["axloader", "build"]).unwrap();

        match cli.command {
            Command::Build(args) => {
                assert_eq!(args.target, "x86_64-unknown-uefi");
                assert!(!args.release);
                assert!(!args.debug);
            }
            _ => panic!("expected build command"),
        }
    }

    #[test]
    fn command_parses_build_debug() {
        let cli = Cli::try_parse_from(["axloader", "build", "--debug"]).unwrap();

        match cli.command {
            Command::Build(args) => {
                assert_eq!(args.target, "x86_64-unknown-uefi");
                assert!(!args.release);
                assert!(args.debug);
            }
            _ => panic!("expected build command"),
        }
    }

    #[test]
    fn command_parses_test_qemu() {
        let cli = Cli::try_parse_from([
            "axloader",
            "test",
            "qemu",
            "--target",
            "x86_64-unknown-uefi",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.target, "x86_64-unknown-uefi");
                }
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn boot_line_includes_qemu_reachable_kernel_url() {
        let boot_line = format_boot_line("x86_64", 4096, 18380);

        assert!(boot_line.starts_with("AXLOADER BOOT "));
        assert!(boot_line.contains("\"kernel_url\":\"http://10.0.2.2:18380/kernel.elf\""));
        assert!(boot_line.contains("\"kernel_size\":4096"));
        assert!(boot_line.contains("\"arch\":\"x86_64\""));
        assert!(boot_line.ends_with('\n'));
    }

    #[test]
    fn x86_64_qemu_uses_network_device_supported_by_ostool_ovmf() {
        let args = x86_64_qemu_args(Path::new("/firmware.fd"), Path::new("/esp"));

        assert!(
            args.windows(2)
                .any(|pair| pair == ["-device", "virtio-net-pci,netdev=net0"])
        );
    }

    #[test]
    fn x86_64_qemu_uses_kvm_host_cpu_for_ovmf_network_stack() {
        let args = x86_64_qemu_args(Path::new("/firmware.fd"), Path::new("/esp"));

        assert!(args.windows(2).any(|pair| pair == ["-accel", "kvm"]));
        assert!(args.windows(2).any(|pair| pair == ["-cpu", "host"]));
    }

    #[test]
    fn ready_near_startup_deadline_gets_a_transfer_window() {
        let started = Instant::now();
        let ready_at = started + HTTP_SMOKE_BOOT_TIMEOUT - Duration::from_millis(1);
        let mut progress = SmokeAttemptProgress::waiting_for_ready(started);

        progress.mark_boot_sent(ready_at);

        assert_eq!(progress.deadline, ready_at + HTTP_SMOKE_TRANSFER_TIMEOUT);
        assert!(!progress.expired_at(ready_at + Duration::from_secs(1)));
    }

    #[test]
    fn slow_ovmf_boot_keeps_a_thirty_second_startup_margin() {
        let started = Instant::now();
        let progress = SmokeAttemptProgress::waiting_for_ready(started);

        assert!(!progress.expired_at(started + Duration::from_secs(195)));
    }

    #[test]
    fn first_failed_qemu_attempt_is_retried() {
        assert_eq!(next_smoke_attempt(1), Some(2));
        assert_eq!(next_smoke_attempt(2), None);
    }
}
