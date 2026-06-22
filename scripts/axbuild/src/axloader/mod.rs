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

use crate::support::process::ProcessExt;

const AXLOADER_PACKAGE: &str = "axloader";
const AXLOADER_BIN: &str = "axloader";
const DEFAULT_UEFI_TARGET: &str = "x86_64-unknown-uefi";
const HTTP_SMOKE_TIMEOUT: Duration = Duration::from_secs(120);
const QEMU_HOST_GATEWAY: &str = "10.0.2.2";
const LEGACY_X86_64_UEFI_FIRMWARE_ENV: &str = "AXVISOR_X86_64_UEFI_FIRMWARE";

#[derive(Clone, Copy)]
struct LoaderSmokeTarget {
    cargo_target: &'static str,
    arch: &'static str,
    efi_output_file: &'static str,
    firmware_env: &'static str,
    firmware_candidates: &'static [&'static str],
    qemu_program: &'static str,
    qemu_args: fn(&Path, &Path) -> Vec<String>,
    kernel_elf: fn() -> Vec<u8>,
}

const X86_64_UEFI_FIRMWARE_CANDIDATES: &[&str] = &[
    "/usr/share/OVMF/OVMF_CODE_4M.fd",
    "/usr/share/OVMF/OVMF_CODE.fd",
    "/usr/share/ovmf/OVMF.fd",
    "/usr/share/qemu/OVMF.fd",
];

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
            Command::Test(args) => test(&self.workspace_root, args),
        }
    }
}

pub fn build(workspace_root: &Path, args: ArgsBuild) -> anyhow::Result<()> {
    run_loader_build(workspace_root, &args.target, args.release || !args.debug)
}

pub fn test(workspace_root: &Path, args: ArgsTest) -> anyhow::Result<()> {
    match args.command {
        TestCommand::Qemu(args) => test_qemu(workspace_root, args),
    }
}

fn test_qemu(workspace_root: &Path, args: ArgsTestQemu) -> anyhow::Result<()> {
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

    run_http_smoke_test(workspace_root, &args.target)
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

fn run_http_smoke_test(workspace_root: &Path, target: &str) -> anyhow::Result<()> {
    let smoke_target = smoke_target(target)?;

    println!("axloader http smoke: building UEFI loader ...");
    run_loader_build(workspace_root, target, true)?;

    let firmware = find_uefi_firmware(smoke_target)?;
    let temp = tempfile::tempdir().context("failed to create axloader HTTP smoke temp dir")?;
    let efi_boot_dir = temp.path().join("esp/EFI/BOOT");
    fs::create_dir_all(&efi_boot_dir)
        .with_context(|| format!("failed to create {}", efi_boot_dir.display()))?;
    fs::copy(
        axloader_efi_path(workspace_root, target),
        efi_boot_dir.join(smoke_target.efi_output_file),
    )
    .context("failed to stage axloader EFI binary")?;

    let kernel = (smoke_target.kernel_elf)();
    let http_server = SmokeHttpServer::start(kernel.clone())?;
    let kernel_url = format!(
        "http://{QEMU_HOST_GATEWAY}:{}/kernel.elf",
        http_server.port()
    );
    let boot_line = format!(
        concat!(
            "AXLOADER BOOT {{",
            "\"protocol_version\":1,",
            "\"boot_id\":\"ci-http-smoke\",",
            "\"kernel_url\":\"{}\",",
            "\"kernel_size\":{},",
            "\"image_format\":\"elf64\",",
            "\"arch\":\"{}\",",
            "\"entry_symbol\":null",
            "}}\n"
        ),
        kernel_url,
        kernel.len(),
        smoke_target.arch,
    );

    println!("axloader http smoke: running QEMU ...");
    let mut child = spawn_axloader_qemu(smoke_target, &firmware, &temp.path().join("esp"))?;
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

    let started = Instant::now();
    let mut transcript = String::new();
    let mut boot_sent = false;
    let mut loaded = false;
    while started.elapsed() < HTTP_SMOKE_TIMEOUT {
        match output_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => {
                print!("{chunk}");
                transcript.push_str(&chunk);
                if !boot_sent && transcript.contains("AXLOADER READY") {
                    stdin
                        .write_all(boot_line.as_bytes())
                        .context("failed to send AXLOADER BOOT over QEMU serial")?;
                    stdin.flush().ok();
                    boot_sent = true;
                }
                if transcript.contains("elf_loaded:") {
                    loaded = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if child.try_wait()?.is_some() {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    stop_child(&mut child);

    if !loaded {
        bail!("axloader HTTP smoke did not reach elf_loaded; transcript:\n{transcript}");
    }
    if !http_server.was_requested() {
        bail!("axloader HTTP smoke reached elf_loaded without observing /kernel.elf request");
    }

    println!("axloader http smoke: kernel transferred and ELF loaded");
    Ok(())
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
            cargo_target: "x86_64-unknown-uefi",
            arch: "x86_64",
            efi_output_file: "BOOTX64.EFI",
            firmware_env: "AXLOADER_X86_64_UEFI_FIRMWARE",
            firmware_candidates: X86_64_UEFI_FIRMWARE_CANDIDATES,
            qemu_program: "qemu-system-x86_64",
            qemu_args: x86_64_qemu_args,
            kernel_elf: minimal_x86_64_kernel_elf,
        }),
        _ => bail!("axloader HTTP smoke does not support target `{target}`"),
    }
}

fn find_uefi_firmware(target: LoaderSmokeTarget) -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os(target.firmware_env) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }
    if target.firmware_env == "AXLOADER_X86_64_UEFI_FIRMWARE"
        && let Some(path) = std::env::var_os(LEGACY_X86_64_UEFI_FIRMWARE_ENV)
    {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }

    for candidate in target.firmware_candidates {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Ok(path);
        }
    }

    bail!(
        "UEFI firmware not found for {}; set {} or install ovmf",
        target.cargo_target,
        target.firmware_env
    )
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
        "-display".into(),
        "none".into(),
        "-monitor".into(),
        "none".into(),
        "-serial".into(),
        "stdio".into(),
        "-netdev".into(),
        "user,id=net0".into(),
        "-device".into(),
        "e1000,netdev=net0".into(),
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
    fn command_rejects_legacy_http_smoke_flag() {
        assert!(Cli::try_parse_from(["axloader", "test", "qemu", "--http-smoke"]).is_err());
    }
}
