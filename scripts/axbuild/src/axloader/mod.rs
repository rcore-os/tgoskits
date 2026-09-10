use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
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
use httpboot_protocol::{
    BootArch, ImageFormat, LoaderDiscoveryOffer, LoaderDiscoveryProbe, LoaderPollResponse,
    LoaderStatusPhase, LoaderStatusReport, PROTOCOL_VERSION,
};
use ostool::ovmf::Arch;
use sha2::{Digest, Sha256};

use crate::support::{ovmf::OvmfFirmware, process::ProcessExt};

const AXLOADER_PACKAGE: &str = "axloader";
const AXLOADER_BIN: &str = "axloader";
const DEFAULT_UEFI_TARGET: &str = "x86_64-unknown-uefi";
const HTTP_SMOKE_BOOT_TIMEOUT: Duration = Duration::from_secs(240);
const HTTP_SMOKE_MAX_ATTEMPTS: usize = 2;
const HTTP_SMOKE_DISCOVERY_REPLY_DELAY: Duration = Duration::from_millis(500);
const QEMU_HOST_GATEWAY: &str = "10.0.2.2";

#[derive(Clone, Copy)]
struct LoaderSmokeTarget {
    arch: &'static str,
    ovmf_arch: Arch,
    efi_output_file: &'static str,
    qemu_program: &'static str,
    qemu_args: fn(&Path, &Path, u16, u16) -> Vec<String>,
    kernel_elf: fn() -> Vec<u8>,
}

struct SmokeAttemptContext<'a> {
    workspace_root: &'a Path,
    target: &'a str,
    smoke_target: LoaderSmokeTarget,
    firmware: &'a Path,
    kernel: &'a [u8],
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

    let control_server =
        SmokeControlServer::start(context.smoke_target.arch, context.kernel.to_vec())?;

    let mut child = spawn_axloader_qemu(
        context.smoke_target,
        context.firmware,
        &temp.path().join("esp"),
        control_server.capture_port(),
        control_server.injection_port(),
    )?;
    let smoke_result = drive_http_smoke_session(&mut child, &control_server);
    stop_child(&mut child);
    smoke_result?;

    if !control_server.was_requested() || !control_server.ready_to_handoff() {
        bail!("axloader network smoke did not observe both kernel download and ready_to_handoff");
    }

    Ok(())
}

fn drive_http_smoke_session(
    child: &mut Child,
    control_server: &SmokeControlServer,
) -> anyhow::Result<()> {
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

    let deadline = Instant::now() + HTTP_SMOKE_BOOT_TIMEOUT;
    let mut transcript = String::new();
    while Instant::now() < deadline {
        if transcript.contains("elf_loaded:")
            && control_server.was_requested()
            && control_server.ready_to_handoff()
        {
            return Ok(());
        }
        match output_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => {
                print!("{chunk}");
                transcript.push_str(&chunk);
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

    bail!("axloader network smoke timed out; transcript:\n{transcript}")
}

fn next_smoke_attempt(current_attempt: usize) -> Option<usize> {
    (current_attempt < HTTP_SMOKE_MAX_ATTEMPTS).then_some(current_attempt + 1)
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
    capture_port: u16,
    injection_port: u16,
) -> anyhow::Result<Child> {
    StdCommand::new(target.qemu_program)
        .args((target.qemu_args)(
            firmware,
            esp_dir,
            capture_port,
            injection_port,
        ))
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

fn x86_64_qemu_args(
    firmware: &Path,
    esp_dir: &Path,
    capture_port: u16,
    injection_port: u16,
) -> Vec<String> {
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
        "host".into(),
        "-display".into(),
        "none".into(),
        "-monitor".into(),
        "none".into(),
        "-serial".into(),
        "stdio".into(),
        "-netdev".into(),
        "user,id=user0".into(),
        "-chardev".into(),
        format!("socket,id=discovery_capture,host=127.0.0.1,port={capture_port},reconnect-ms=100"),
        "-chardev".into(),
        format!(
            "socket,id=discovery_injection,host=127.0.0.1,port={injection_port},reconnect-ms=100"
        ),
        "-object".into(),
        "filter-mirror,id=discovery_mirror,netdev=user0,queue=rx,outdev=discovery_capture".into(),
        "-object".into(),
        "filter-redirector,id=discovery_redirect,netdev=user0,queue=tx,indev=discovery_injection"
            .into(),
        "-device".into(),
        // ostool's OVMF prebuilt always includes VirtioNetDxe, while its E1000
        // driver is optional and absent from the pinned firmware build.
        "virtio-net-pci,netdev=user0,mac=02:00:00:00:00:01".into(),
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

struct SmokeControlServer {
    stop: Arc<AtomicBool>,
    requested: Arc<AtomicBool>,
    ready_to_handoff: Arc<AtomicBool>,
    threads: Vec<thread::JoinHandle<()>>,
    capture_port: u16,
    injection_port: u16,
}

impl SmokeControlServer {
    fn start(arch: &str, body: Vec<u8>) -> anyhow::Result<Self> {
        let listener =
            TcpListener::bind("0.0.0.0:0").context("failed to bind axloader control server")?;
        let port = listener
            .local_addr()
            .context("failed to read axloader control server address")?
            .port();
        listener
            .set_nonblocking(true)
            .context("failed to configure axloader control server")?;
        let capture_listener = TcpListener::bind("127.0.0.1:0")
            .context("failed to bind axloader discovery capture")?;
        let capture_port = capture_listener.local_addr()?.port();
        capture_listener
            .set_nonblocking(true)
            .context("failed to configure axloader discovery capture")?;
        let injection_listener = TcpListener::bind("127.0.0.1:0")
            .context("failed to bind axloader discovery injection")?;
        let injection_port = injection_listener.local_addr()?.port();
        injection_listener
            .set_nonblocking(true)
            .context("failed to configure axloader discovery injection")?;

        let stop = Arc::new(AtomicBool::new(false));
        let requested = Arc::new(AtomicBool::new(false));
        let ready_to_handoff = Arc::new(AtomicBool::new(false));
        let http_stop = stop.clone();
        let http_requested = requested.clone();
        let http_ready = ready_to_handoff.clone();
        let kernel_sha256 = format!("{:x}", Sha256::digest(&body));
        let boot_arch = parse_smoke_arch(arch)?;
        let http_thread = thread::spawn(move || {
            while !http_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0u8; 16 * 1024];
                        let read = stream.read(&mut request).unwrap_or(0);
                        let request = &request[..read];
                        let first_line_end = request
                            .windows(2)
                            .position(|window| window == b"\r\n")
                            .unwrap_or(request.len());
                        let first_line = String::from_utf8_lossy(&request[..first_line_end]);
                        if first_line.starts_with("GET /kernel.elf ") {
                            http_requested.store(true, Ordering::Release);
                            write_http_response(&mut stream, "200 OK", &body);
                        } else if first_line.starts_with("POST /api/v1/loaders/poll ") {
                            if !json_request_is_framed(request) {
                                write_http_response(&mut stream, "400 Bad Request", &[]);
                                continue;
                            }
                            let response = LoaderPollResponse::Boot {
                                board_id: "qemu-smoke".into(),
                                session_id: "qemu-smoke-session".into(),
                                boot_id: "qemu-smoke-boot".into(),
                                kernel_path: "/kernel.elf".into(),
                                kernel_size: body.len() as u64,
                                kernel_sha256: kernel_sha256.clone(),
                                arch: boot_arch,
                                image_format: ImageFormat::Elf64,
                                entry_symbol: None,
                            };
                            let response = serde_json::to_vec(&response).unwrap();
                            write_http_response(&mut stream, "200 OK", &response);
                        } else if first_line.starts_with("POST /api/v1/loaders/status ") {
                            if !json_request_is_framed(request) {
                                write_http_response(&mut stream, "400 Bad Request", &[]);
                                continue;
                            }
                            if let Some(body_start) = request
                                .windows(4)
                                .position(|window| window == b"\r\n\r\n")
                                .map(|offset| offset + 4)
                                && let Ok(report) = serde_json::from_slice::<LoaderStatusReport>(
                                    &request[body_start..],
                                )
                                && report.status == LoaderStatusPhase::ReadyToHandoff
                            {
                                http_ready.store(true, Ordering::Release);
                            }
                            write_http_response(&mut stream, "204 No Content", &[]);
                        } else {
                            write_http_response(&mut stream, "404 Not Found", &[]);
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        let udp_stop = stop.clone();
        let udp_thread = thread::spawn(move || {
            let mut capture = None;
            let mut injection = None;
            let mut encoded_frames = Vec::new();
            let mut buffer = [0u8; 2048];
            while !udp_stop.load(Ordering::Acquire) {
                accept_qemu_filter(&capture_listener, &mut capture);
                accept_qemu_filter(&injection_listener, &mut injection);
                let Some(stream) = capture.as_mut() else {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                };
                match stream.read(&mut buffer) {
                    Ok(0) => capture = None,
                    Ok(read) => encoded_frames.extend_from_slice(&buffer[..read]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => capture = None,
                }

                while let Some(frame) = take_qemu_filter_frame(&mut encoded_frames) {
                    let Some(request) = parse_discovery_frame(&frame) else {
                        continue;
                    };
                    let offer = LoaderDiscoveryOffer {
                        protocol_version: PROTOCOL_VERSION,
                        server_id: "axloader-qemu-smoke".into(),
                        control_base_url: format!("http://{QEMU_HOST_GATEWAY}:{port}"),
                        registration_id: "axloader-qemu-registration".into(),
                        expires_in_ms: 60_000,
                    };
                    let payload = serde_json::to_vec(&offer).unwrap();
                    let response = build_discovery_reply(&request, &payload);
                    // Let SLiRP report its unhandled copy of the broadcast
                    // first.  The loader must keep receiving after that ICMP
                    // error and still accept this valid discovery offer.
                    thread::sleep(HTTP_SMOKE_DISCOVERY_REPLY_DELAY);
                    if let Some(stream) = injection.as_mut()
                        && write_qemu_filter_frame(stream, &response).is_err()
                    {
                        injection = None;
                    }
                }
                thread::sleep(Duration::from_millis(10));
            }
        });

        println!(
            "axloader network smoke: discovery filter capture :{capture_port}, injection \
             :{injection_port}, and HTTP control :{port}"
        );
        Ok(Self {
            stop,
            requested,
            ready_to_handoff,
            threads: vec![http_thread, udp_thread],
            capture_port,
            injection_port,
        })
    }

    fn was_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    fn ready_to_handoff(&self) -> bool {
        self.ready_to_handoff.load(Ordering::Acquire)
    }

    fn capture_port(&self) -> u16 {
        self.capture_port
    }

    fn injection_port(&self) -> u16 {
        self.injection_port
    }
}

impl Drop for SmokeControlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn accept_qemu_filter(listener: &TcpListener, stream: &mut Option<TcpStream>) {
    if stream.is_some() {
        return;
    }
    match listener.accept() {
        Ok((accepted, _)) => {
            let _ = accepted.set_nonblocking(true);
            let _ = accepted.set_nodelay(true);
            *stream = Some(accepted);
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(_) => {}
    }
}

fn take_qemu_filter_frame(encoded: &mut Vec<u8>) -> Option<Vec<u8>> {
    const MAX_FRAME_BYTES: usize = 64 * 1024;
    if encoded.len() < std::mem::size_of::<u32>() {
        return None;
    }
    let frame_length = u32::from_be_bytes(encoded[..4].try_into().ok()?) as usize;
    if frame_length == 0 || frame_length > MAX_FRAME_BYTES {
        encoded.clear();
        return None;
    }
    let encoded_length = std::mem::size_of::<u32>() + frame_length;
    if encoded.len() < encoded_length {
        return None;
    }
    let frame = encoded[4..encoded_length].to_vec();
    encoded.drain(..encoded_length);
    Some(frame)
}

fn write_qemu_filter_frame(stream: &mut TcpStream, frame: &[u8]) -> std::io::Result<()> {
    let frame_length = u32::try_from(frame.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Ethernet frame is too large",
        )
    })?;
    stream.write_all(&frame_length.to_be_bytes())?;
    stream.write_all(frame)
}

struct DiscoveryFrame {
    guest_mac: [u8; 6],
    guest_ip: [u8; 4],
    guest_port: u16,
}

fn parse_discovery_frame(frame: &[u8]) -> Option<DiscoveryFrame> {
    if frame.len() < 14 + 20 + 8 || frame.get(12..14)? != [0x08, 0x00] {
        return None;
    }
    let ip = 14;
    let header_length = usize::from(frame[ip] & 0x0f) * 4;
    if frame[ip] >> 4 != 4 || header_length < 20 || frame[ip + 9] != 17 {
        return None;
    }
    let total_length = usize::from(u16::from_be_bytes([frame[ip + 2], frame[ip + 3]]));
    if total_length < header_length + 8 || ip.checked_add(total_length)? > frame.len() {
        return None;
    }
    let udp = ip + header_length;
    let destination_port = u16::from_be_bytes([frame[udp + 2], frame[udp + 3]]);
    let udp_length = usize::from(u16::from_be_bytes([frame[udp + 4], frame[udp + 5]]));
    if destination_port != httpboot_protocol::DISCOVERY_PORT
        || udp_length < 8
        || udp.checked_add(udp_length)? > frame.len()
    {
        return None;
    }
    serde_json::from_slice::<LoaderDiscoveryProbe>(&frame[udp + 8..udp + udp_length]).ok()?;
    Some(DiscoveryFrame {
        guest_mac: frame.get(6..12)?.try_into().ok()?,
        guest_ip: frame.get(ip + 12..ip + 16)?.try_into().ok()?,
        guest_port: u16::from_be_bytes([frame[udp], frame[udp + 1]]),
    })
}

fn build_discovery_reply(request: &DiscoveryFrame, payload: &[u8]) -> Vec<u8> {
    const ETHERNET_HEADER: usize = 14;
    const IPV4_HEADER: usize = 20;
    const UDP_HEADER: usize = 8;
    // QEMU's SLiRP gateway MAC for the default 10.0.2.0/24 network.  Using the
    // gateway identity makes the injected discovery response consistent with
    // the neighbour entry that the same interface later uses for HTTP.
    const SERVER_MAC: [u8; 6] = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];
    let ip_length = IPV4_HEADER + UDP_HEADER + payload.len();
    let mut frame = vec![0u8; (ETHERNET_HEADER + ip_length).max(60)];
    frame[..6].copy_from_slice(&request.guest_mac);
    frame[6..12].copy_from_slice(&SERVER_MAC);
    frame[12..14].copy_from_slice(&[0x08, 0x00]);

    let ip = ETHERNET_HEADER;
    frame[ip] = 0x45;
    frame[ip + 2..ip + 4].copy_from_slice(&(ip_length as u16).to_be_bytes());
    frame[ip + 6..ip + 8].copy_from_slice(&0x4000_u16.to_be_bytes());
    frame[ip + 8] = 64;
    frame[ip + 9] = 17;
    frame[ip + 12..ip + 16].copy_from_slice(&[10, 0, 2, 2]);
    frame[ip + 16..ip + 20].copy_from_slice(&request.guest_ip);
    let checksum = ipv4_checksum(&frame[ip..ip + IPV4_HEADER]);
    frame[ip + 10..ip + 12].copy_from_slice(&checksum.to_be_bytes());

    let udp = ip + IPV4_HEADER;
    frame[udp..udp + 2].copy_from_slice(&httpboot_protocol::DISCOVERY_PORT.to_be_bytes());
    frame[udp + 2..udp + 4].copy_from_slice(&request.guest_port.to_be_bytes());
    let udp_length = UDP_HEADER + payload.len();
    frame[udp + 4..udp + 6].copy_from_slice(&(udp_length as u16).to_be_bytes());
    frame[udp + UDP_HEADER..udp + UDP_HEADER + payload.len()].copy_from_slice(payload);

    let mut checksum_input = Vec::with_capacity(12 + udp_length);
    checksum_input.extend_from_slice(&frame[ip + 12..ip + 20]);
    checksum_input.push(0);
    checksum_input.push(frame[ip + 9]);
    checksum_input.extend_from_slice(&(udp_length as u16).to_be_bytes());
    checksum_input.extend_from_slice(&frame[udp..udp + udp_length]);
    let checksum = internet_checksum(&checksum_input);
    frame[udp + 6..udp + 8].copy_from_slice(&checksum.to_be_bytes());
    frame
}

fn ipv4_checksum(header: &[u8]) -> u16 {
    internet_checksum(header)
}

fn internet_checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0u32;
    let (pairs, remainder) = bytes.as_chunks::<2>();
    for pair in pairs {
        sum += u32::from(u16::from_be_bytes([pair[0], pair[1]]));
    }
    if let Some(byte) = remainder.first() {
        sum += u32::from(*byte) << 8;
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn write_http_response(stream: &mut impl Write, status: &str, body: &[u8]) {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    if stream.write_all(header.as_bytes()).is_ok() {
        let _ = stream.write_all(body);
    }
}

fn json_request_is_framed(request: &[u8]) -> bool {
    let Some(body_start) = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|offset| offset + 4)
    else {
        return false;
    };
    let Ok(headers) = std::str::from_utf8(&request[..body_start]) else {
        return false;
    };
    let mut content_type_is_json = false;
    let mut content_length = None;
    for line in headers.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-type") {
            content_type_is_json = value.trim().eq_ignore_ascii_case("application/json");
        } else if name.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    content_type_is_json && content_length == Some(request.len() - body_start)
}

fn parse_smoke_arch(arch: &str) -> anyhow::Result<BootArch> {
    match arch {
        "x86_64" => Ok(BootArch::X86_64),
        "aarch64" => Ok(BootArch::Aarch64),
        "riscv64" => Ok(BootArch::Riscv64),
        "loongarch64" => Ok(BootArch::Loongarch64),
        _ => bail!("unsupported axloader smoke architecture `{arch}`"),
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

    use super::*;

    #[test]
    fn smoke_arch_uses_protocol_architecture() {
        assert_eq!(parse_smoke_arch("x86_64").unwrap(), BootArch::X86_64);
        assert!(parse_smoke_arch("mips64").is_err());
    }

    #[test]
    fn x86_64_qemu_uses_network_device_supported_by_ostool_ovmf() {
        let args = x86_64_qemu_args(Path::new("/firmware.fd"), Path::new("/esp"), 12345, 12346);

        assert!(args.windows(2).any(|pair| pair
            == [
                "-device",
                "virtio-net-pci,netdev=user0,mac=02:00:00:00:00:01"
            ]));
    }

    #[test]
    fn x86_64_qemu_mirrors_discovery_frames_without_serial_control() {
        let args = x86_64_qemu_args(Path::new("/firmware.fd"), Path::new("/esp"), 12345, 12346);

        assert!(args.windows(2).any(|pair| pair == ["-machine", "q35"]));
        assert!(args.windows(2).any(|pair| pair == ["-cpu", "host"]));
        assert!(args.iter().any(|arg| arg
            == "filter-mirror,id=discovery_mirror,netdev=user0,queue=rx,outdev=discovery_capture"));
        assert!(args.iter().any(|arg| arg
            == "filter-redirector,id=discovery_redirect,netdev=user0,queue=tx,\
                indev=discovery_injection"));
    }

    #[test]
    fn first_failed_qemu_attempt_is_retried() {
        assert_eq!(next_smoke_attempt(1), Some(2));
        assert_eq!(next_smoke_attempt(2), None);
    }

    #[test]
    fn json_control_requests_require_explicit_http_framing() {
        let body = br#"{"protocol_version":2}"#;
        let content_length = format!("Content-Length: {}\r\n\r\n", body.len());
        let framed = [
            b"POST /api/v1/loaders/poll HTTP/1.1\r\n".as_slice(),
            b"Content-Type: application/json\r\n",
            content_length.as_bytes(),
            body,
        ]
        .concat();
        assert!(json_request_is_framed(&framed));

        let unframed = [
            b"POST /api/v1/loaders/poll HTTP/1.1\r\n".as_slice(),
            b"Host: 10.0.2.2\r\n\r\n",
            body,
        ]
        .concat();
        assert!(!json_request_is_framed(&unframed));
    }

    #[test]
    fn qemu_filter_frame_parser_waits_for_complete_frame_and_rejects_invalid_lengths() {
        let mut encoded = Vec::from(3_u32.to_be_bytes());
        encoded.extend_from_slice(b"ab");
        assert!(take_qemu_filter_frame(&mut encoded).is_none());
        encoded.push(b'c');
        assert_eq!(take_qemu_filter_frame(&mut encoded).unwrap(), b"abc");
        assert!(encoded.is_empty());

        let mut empty = Vec::from(0_u32.to_be_bytes());
        assert!(take_qemu_filter_frame(&mut empty).is_none());
        assert!(empty.is_empty());
    }

    #[test]
    fn discovery_reply_targets_requester_and_has_valid_checksums() {
        let request = DiscoveryFrame {
            guest_mac: [0x02, 0, 0, 0, 0, 1],
            guest_ip: [10, 0, 2, 15],
            guest_port: 2999,
        };
        let frame = build_discovery_reply(&request, b"offer");

        assert_eq!(&frame[..6], &request.guest_mac);
        assert_eq!(ipv4_checksum(&frame[14..34]), 0);
        assert_eq!(u16::from_be_bytes([frame[34], frame[35]]), 2998);
        assert_eq!(
            u16::from_be_bytes([frame[36], frame[37]]),
            request.guest_port
        );

        let udp_length = usize::from(u16::from_be_bytes([frame[38], frame[39]]));
        let mut checksum_input = Vec::new();
        checksum_input.extend_from_slice(&frame[26..34]);
        checksum_input.extend_from_slice(&[0, 17]);
        checksum_input.extend_from_slice(&(udp_length as u16).to_be_bytes());
        checksum_input.extend_from_slice(&frame[34..34 + udp_length]);
        assert_eq!(internet_checksum(&checksum_input), 0);
    }
}
