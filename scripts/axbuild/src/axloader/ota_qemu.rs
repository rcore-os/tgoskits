//! Persistent FAT image OTA checks using real OVMF TCP4 and host forwarding.

use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream, UdpSocket},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail, ensure};
use ostool::ovmf::Arch;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::support::ovmf::OvmfFirmware;

const STATUS_TIMEOUT: Duration = Duration::from_secs(75);

struct QemuChild(Child);

impl Drop for QemuChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn command(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to launch {program}"))?;
    ensure!(
        output.status.success(),
        "{program} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn start_qemu(
    firmware: &OvmfFirmware,
    root: &Path,
    port: u16,
    server_filters: Option<(u16, u16, u16)>,
) -> anyhow::Result<QemuChild> {
    let serial = fs::File::create(root.join("ota-qemu.log"))?;
    let vars = root.join("vars.fd");
    let disk = root.join("esp.img");
    let mut command = Command::new("qemu-system-x86_64");
    let netdev = if let Some((_, _, udp_port)) = server_filters {
        format!(
            "user,id=user0,hostfwd=tcp:127.0.0.1:{port}-:2999,hostfwd=udp:127.0.0.1:{udp_port}-:\
             2999"
        )
    } else {
        format!("user,id=user0,hostfwd=tcp:127.0.0.1:{port}-:2999")
    };
    command.args([
        "-m", "256M", "-smp", "1", "-machine", "q35", "-accel", "kvm", "-cpu", "host", "-display",
        "none", "-monitor", "none", "-serial", "stdio", "-netdev", &netdev,
    ]);
    if let Some((capture, inject, _)) = server_filters {
        command.args([
            "-chardev",
            &format!("socket,id=discovery_capture,host=127.0.0.1,port={capture},reconnect-ms=100"),
            "-chardev",
            &format!("socket,id=discovery_injection,host=127.0.0.1,port={inject},reconnect-ms=100"),
            "-object",
            "filter-mirror,id=discovery_mirror,netdev=user0,queue=rx,outdev=discovery_capture",
            "-object",
            "filter-redirector,id=discovery_redirect,netdev=user0,queue=tx,\
             indev=discovery_injection",
        ]);
    }
    command.args([
        "-device",
        "virtio-net-pci,netdev=user0,mac=02:00:00:00:00:01",
        "-drive",
        &format!(
            "if=pflash,format=raw,readonly=on,file={}",
            firmware.code().display()
        ),
        "-drive",
        &format!("if=pflash,format=raw,file={}", vars.display()),
        "-drive",
        &format!("format=raw,if=ide,file={}", disk.display()),
    ]);
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(serial))
        .stderr(Stdio::null())
        .spawn()
        .context("failed to start OTA OVMF guest")?;
    Ok(QemuChild(child))
}

async fn status(
    client: &reqwest::Client,
    port: u16,
    root: &Path,
    predicate: impl Fn(&Value) -> bool,
) -> anyhow::Result<Value> {
    let deadline = Instant::now() + STATUS_TIMEOUT;
    let url = format!("http://127.0.0.1:{port}/api/v1/ota/status");
    let mut last = String::new();
    while Instant::now() < deadline {
        match client.get(&url).send().await {
            Ok(response) => match response.json::<Value>().await {
                Ok(value) => {
                    if predicate(&value) {
                        return Ok(value);
                    }
                    last = value.to_string();
                }
                Err(error) => last = error.to_string(),
            },
            Err(error) => last = error.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let log = fs::read_to_string(root.join("ota-qemu.log")).unwrap_or_default();
    bail!(
        "OTA status timed out: {last}; QEMU serial tail:\n{}",
        log.chars()
            .rev()
            .take(2048)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>()
    )
}

async fn upload(
    client: &reqwest::Client,
    port: u16,
    bytes: &[u8],
    digest: &str,
) -> anyhow::Result<reqwest::Response> {
    client
        .put(format!("http://127.0.0.1:{port}/api/v1/ota/image"))
        .header("X-Image-Sha256", digest)
        .body(bytes.to_vec())
        .send()
        .await
        .context("direct OTA upload failed")
}

pub(super) async fn test_direct_ota(
    workspace: &Path,
    target_dir: &Path,
    target: &str,
    server_only: bool,
) -> anyhow::Result<()> {
    if target != super::DEFAULT_UEFI_TARGET {
        bail!("persistent OTA QEMU test supports only x86_64 UEFI");
    }
    let root = tempfile::tempdir().context("failed to create OTA FAT image directory")?;
    let root = root.path();
    let firmware = OvmfFirmware::fetch(Arch::X64).await?;
    fs::copy(firmware.vars(), root.join("vars.fd"))?;
    let loader = fs::read(super::axloader_efi_path(target_dir, target))?;
    // Distinct, valid PE files represent the old and the first new loader.
    let mut old_loader = loader.clone();
    old_loader.push(0x01);
    fs::write(root.join("A.EFI"), &old_loader)?;
    fs::write(root.join("B.EFI"), &loader)?;
    fs::copy(
        target_dir
            .join(target)
            .join("release/axloader-launcher.efi"),
        root.join("BOOTX64.EFI"),
    )?;
    let output = Command::new("python3")
        .arg(workspace.join("bootloader/axloader/scripts/init-ota-state.py"))
        .args(["--stable", "A.EFI", "--trial", "B.EFI", "--output", "."])
        .current_dir(root)
        .output()?;
    ensure!(
        output.status.success(),
        "failed to initialize OTA records: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let migration = String::from_utf8(output.stdout)?;
    let initial_id = migration
        .trim()
        .strip_prefix("first trial update_id=")
        .context("missing migration ID")?;
    if server_only {
        fs::write(root.join("STATE1.BIN"), [0; 256])?;
    }

    let disk = root.join("esp.img");
    let disk_path = disk.to_str().context("non-UTF8 FAT path")?;
    command("truncate", &["-s", "128M", disk_path])?;
    command("mkfs.vfat", &["-F", "32", "-n", "OSTOOLBOOT", disk_path])?;
    command(
        "mmd",
        &["-i", disk_path, "::EFI", "::EFI/BOOT", "::EFI/AXLOADER"],
    )?;
    command(
        "mcopy",
        &[
            "-i",
            disk_path,
            root.join("BOOTX64.EFI").to_str().unwrap(),
            "::EFI/BOOT/BOOTX64.EFI",
        ],
    )?;
    for name in ["A.EFI", "B.EFI", "STATE0.BIN", "STATE1.BIN"] {
        command(
            "mcopy",
            &[
                "-i",
                disk_path,
                root.join(name).to_str().unwrap(),
                &format!("::EFI/AXLOADER/{name}"),
            ],
        )?;
    }
    let port = TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(35))
        .build()?;
    if server_only {
        let server = AssignedOtaServer::start(loader.clone())?;
        let qemu = start_qemu(&firmware, root, port, Some(server.filter_ports()))?;
        let assigned = status(&client, port, root, |value| {
            value["running_slot"] == "b"
                && value["active_slot"] == "b"
                && value["last_outcome"] == "confirmed"
        })
        .await?;
        ensure!(
            assigned["running_sha256"] == format!("{:x}", Sha256::digest(&loader))
                && server.confirmed.load(Ordering::Acquire),
            "v4 server assignment did not commit: {assigned}"
        );
        drop(qemu);
        println!("axloader OTA QEMU: isolated v4 server assignment passed");
        return Ok(());
    }
    let mut qemu = start_qemu(&firmware, root, port, None)?;
    let status0 = status(&client, port, root, |value| {
        value["pending_update_id"] == initial_id && value["trial"] == true
    })
    .await?;
    ensure!(
        status0["running_slot"] == "b" && status0["active_slot"] == "a",
        "first trial is not B: {status0}"
    );
    let confirmed = client
        .post(format!("http://127.0.0.1:{port}/api/v1/ota/confirm"))
        .json(&serde_json::json!({"update_id": initial_id}))
        .send()
        .await?;
    ensure!(
        confirmed.status().is_success(),
        "first trial confirmation failed: {}",
        confirmed.status()
    );
    status(&client, port, root, |value| {
        value["active_slot"] == "b" && value["pending_update_id"].is_null()
    })
    .await?;

    let digest = format!("{:x}", Sha256::digest(&old_loader));
    let wrong = upload(&client, port, &old_loader, &"0".repeat(64)).await?;
    ensure!(
        wrong.status() == reqwest::StatusCode::UNPROCESSABLE_ENTITY,
        "bad image SHA was accepted"
    );
    let mut short = TcpStream::connect(format!("127.0.0.1:{port}"))?;
    short.write_all(b"PUT /api/v1/ota/image HTTP/1.1\r\nHost: localhost\r\nContent-Length: 10000\r\nX-Image-Sha256: 0000000000000000000000000000000000000000000000000000000000000000\r\n\r\nhello")?;
    short.shutdown(std::net::Shutdown::Write)?;
    drop(short);
    status(&client, port, root, |value| {
        value["active_slot"] == "b" && value["pending_update_id"].is_null()
    })
    .await?;

    let staged = upload(&client, port, &old_loader, &digest).await?;
    ensure!(
        staged.status() == reqwest::StatusCode::ACCEPTED,
        "OTA upload failed: {}",
        staged.status()
    );
    let id = staged.json::<Value>().await?["update_id"]
        .as_str()
        .context("missing update ID")?
        .to_owned();
    status(&client, port, root, |value| {
        value["trial"] == true && value["running_slot"] == "a" && value["pending_update_id"] == id
    })
    .await?;
    // A killed trial retains its attempted record on the *same* FAT image.
    drop(qemu);
    qemu = start_qemu(&firmware, root, port, None)?;
    let rolled = status(&client, port, root, |value| {
        value["last_outcome"] == "rolled_back" && value["running_slot"] == "b"
    })
    .await?;
    ensure!(
        rolled["active_slot"] == "b" && rolled["pending_update_id"].is_null(),
        "wrong rollback: {rolled}"
    );

    let staged = upload(&client, port, &old_loader, &digest).await?;
    ensure!(
        staged.status() == reqwest::StatusCode::ACCEPTED,
        "second OTA upload failed"
    );
    let id = staged.json::<Value>().await?["update_id"]
        .as_str()
        .context("missing second ID")?
        .to_owned();
    status(&client, port, root, |value| {
        value["pending_update_id"] == id && value["running_slot"] == "a"
    })
    .await?;
    let confirmed = client
        .post(format!("http://127.0.0.1:{port}/api/v1/ota/confirm"))
        .json(&serde_json::json!({"update_id": id}))
        .send()
        .await?;
    ensure!(
        confirmed.status().is_success(),
        "second confirmation failed"
    );
    drop(qemu);
    let qemu = start_qemu(&firmware, root, port, None)?;
    let stable = status(&client, port, root, |value| {
        value["active_slot"] == "a"
            && value["running_slot"] == "a"
            && value["pending_update_id"].is_null()
    })
    .await?;
    ensure!(
        stable["last_outcome"] == "confirmed",
        "confirmed state not persistent: {stable}"
    );
    println!(
        "axloader OTA QEMU: real FAT direct upload, bad SHA, short body, trial reset rollback, \
         and confirmation persistence passed"
    );
    drop(qemu);
    // A complete inactive-slot file without a new record must not be started.
    let mut uncommitted = loader.clone();
    uncommitted.push(0x42);
    fs::write(root.join("UNCOMMITTED.EFI"), &uncommitted)?;
    command(
        "mcopy",
        &[
            "-o",
            "-i",
            disk_path,
            root.join("UNCOMMITTED.EFI").to_str().unwrap(),
            "::EFI/AXLOADER/B.EFI",
        ],
    )?;
    let qemu = start_qemu(&firmware, root, port, None)?;
    status(&client, port, root, |value| {
        value["running_slot"] == "a"
            && value["active_slot"] == "a"
            && value["pending_update_id"].is_null()
    })
    .await?;
    drop(qemu);
    // A valid pending record pointing at a file which cannot be loaded must
    // be committed as a failed trial before returning to the stable slot.
    stage_unloadable_trial(root, disk_path)?;
    let qemu = start_qemu(&firmware, root, port, None)?;
    let failed = status(&client, port, root, |value| {
        value["running_slot"] == "a"
            && value["active_slot"] == "a"
            && value["pending_update_id"].is_null()
            && value["last_outcome"] == "failed"
    })
    .await?;
    ensure!(
        failed["last_failure_reason"] == "trial_image_load_or_digest_failed",
        "unloadable EFI trial did not record the cause: {failed}"
    );
    drop(qemu);
    let server = AssignedOtaServer::start(loader.clone())?;
    let qemu = start_qemu(&firmware, root, port, Some(server.filter_ports()))?;
    let assigned = status(&client, port, root, |value| {
        value["running_slot"] == "b"
            && value["active_slot"] == "b"
            && value["last_outcome"] == "confirmed"
            && value["source"] == "server"
    })
    .await?;
    ensure!(
        assigned["running_sha256"] == format!("{:x}", Sha256::digest(&loader)),
        "server upgrade image mismatch: {assigned}"
    );
    ensure!(
        server.confirmed.load(Ordering::Acquire),
        "v4 server did not confirm the assigned trial"
    );
    drop(qemu);
    let _qemu = start_qemu(&firmware, root, port, Some(server.filter_ports()))?;
    status(&client, port, root, |value| {
        value["running_slot"] == "b"
            && value["active_slot"] == "b"
            && value["pending_update_id"].is_null()
    })
    .await?;
    println!(
        "axloader OTA QEMU: v4 server assignment, report, confirmation, and subsequent FAT boot \
         passed"
    );
    Ok(())
}

fn stage_unloadable_trial(root: &Path, disk_path: &str) -> anyhow::Result<()> {
    let invalid = vec![0x55; 4096];
    let invalid_path = root.join("UNLOADABLE.EFI");
    fs::write(&invalid_path, &invalid)?;
    command(
        "mcopy",
        &[
            "-o",
            "-i",
            disk_path,
            invalid_path.to_str().unwrap(),
            "::EFI/AXLOADER/B.EFI",
        ],
    )?;
    let records = [root.join("STATE0.BIN"), root.join("STATE1.BIN")];
    for (index, path) in records.iter().enumerate() {
        command(
            "mcopy",
            &[
                "-o",
                "-i",
                disk_path,
                &format!("::EFI/AXLOADER/STATE{index}.BIN"),
                path.to_str().unwrap(),
            ],
        )?;
    }
    let records = [fs::read(&records[0])?, fs::read(&records[1])?];
    ensure!(
        records.iter().all(|record| record.len() == 256),
        "invalid OTA record"
    );
    let generation = |record: &[u8]| u64::from_le_bytes(record[8..16].try_into().unwrap());
    let latest = usize::from(generation(&records[1]) > generation(&records[0]));
    let mut next = records[latest].clone();
    ensure!(next[16] == 0 && next[17] == 0xff, "expected stable A slot");
    let next_generation = generation(&next)
        .checked_add(1)
        .context("OTA generation overflow")?;
    next[8..16].copy_from_slice(&next_generation.to_le_bytes());
    next[17] = 1;
    next[18] = 0;
    next[19] = 0;
    next[52..84].copy_from_slice(&Sha256::digest(&invalid));
    next[84..120].copy_from_slice(b"01234567-89ab-cdef-0123-456789abcdef");
    next[120] = 0;
    let checksum = Sha256::digest(&next[..224]);
    next[224..256].copy_from_slice(&checksum);
    let target = 1 - latest;
    let staged = root.join(format!("STAGE{target}.BIN"));
    fs::write(&staged, next)?;
    command(
        "mcopy",
        &[
            "-o",
            "-i",
            disk_path,
            staged.to_str().unwrap(),
            &format!("::EFI/AXLOADER/STATE{target}.BIN"),
        ],
    )
}

struct AssignedOtaServer {
    stop: Arc<AtomicBool>,
    confirmed: Arc<AtomicBool>,
    threads: Vec<thread::JoinHandle<()>>,
    capture: u16,
    inject: u16,
    udp_hostfwd: u16,
}

fn write_ota_response(stream: &mut impl Write, status: &str, body: &[u8]) {
    super::write_http_response_version(stream, status, body, "HTTP/1.0");
}

impl AssignedOtaServer {
    fn start(image: Vec<u8>) -> anyhow::Result<Self> {
        const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
        let listener = TcpListener::bind("0.0.0.0:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let capture_listener = TcpListener::bind("127.0.0.1:0")?;
        capture_listener.set_nonblocking(true)?;
        let capture = capture_listener.local_addr()?.port();
        let injection_listener = TcpListener::bind("127.0.0.1:0")?;
        injection_listener.set_nonblocking(true)?;
        let inject = injection_listener.local_addr()?.port();
        let reservation = UdpSocket::bind("127.0.0.1:0")?;
        let udp_hostfwd = reservation.local_addr()?.port();
        drop(reservation);
        let stop = Arc::new(AtomicBool::new(false));
        let confirmed = Arc::new(AtomicBool::new(false));
        let polled = Arc::new(AtomicBool::new(false));
        let http_stop = stop.clone();
        let http_confirmed = confirmed.clone();
        let http_polled = polled.clone();
        let digest = format!("{:x}", Sha256::digest(&image));
        let http_thread = thread::spawn(move || {
            while !http_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
                        let mut request = [0_u8; 16 * 1024];
                        let count = stream.read(&mut request).unwrap_or(0);
                        let body = &request[..count];
                        let line = String::from_utf8_lossy(body);
                        if line.starts_with("GET /api/v1/loader-updates/") {
                            write_ota_response(&mut stream, "200 OK", &image);
                        } else if line.starts_with("POST /api/v1/loaders/ota-status ") {
                            if line.contains("\"phase\":\"succeeded\"") {
                                http_confirmed.store(true, Ordering::Release);
                            }
                            write_ota_response(&mut stream, "204 No Content", &[]);
                        } else if line.starts_with("POST /api/v1/loaders/poll ") {
                            http_polled.store(true, Ordering::Release);
                            println!("axloader OTA mock: v4 poll received");
                            let body_start = body
                                .windows(4)
                                .position(|part| part == b"\r\n\r\n")
                                .map(|index| index + 4);
                            let state = body_start
                                .and_then(|index| {
                                    serde_json::from_slice::<Value>(&body[index..]).ok()
                                })
                                .unwrap_or_default();
                            let ota = &state["ota"];
                            let response = if ota["trial"] == true
                                && ota["source"] == "server"
                                && ota["pending_update_id"] == ID
                                && ota["running_sha256"] == digest
                            {
                                serde_json::json!({"state": "confirm_update", "board_id": "qemu-ota", "update_id": ID})
                            } else if ota["active_sha256"] == digest {
                                serde_json::json!({"state": "bound_idle", "board_id": "qemu-ota"})
                            } else {
                                serde_json::json!({"state": "update", "board_id": "qemu-ota", "update_id": ID,
                                    "image_path": format!("/api/v1/loader-updates/{ID}/image"),
                                    "image_size": image.len(), "image_sha256": digest})
                            };
                            write_ota_response(
                                &mut stream,
                                "200 OK",
                                response.to_string().as_bytes(),
                            );
                        } else {
                            write_ota_response(&mut stream, "404 Not Found", &[]);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        let udp_stop = stop.clone();
        let udp_polled = polled;
        let udp_thread = thread::spawn(move || {
            let udp_forward = UdpSocket::bind("127.0.0.1:0").expect("local UDP socket");
            let mut capture_stream = None;
            let mut inject_stream = None;
            let mut frames = Vec::new();
            let mut buf = [0_u8; 2048];
            while !udp_stop.load(Ordering::Acquire) {
                super::accept_qemu_filter(&capture_listener, &mut capture_stream);
                super::accept_qemu_filter(&injection_listener, &mut inject_stream);
                if let Some(stream) = capture_stream.as_mut() {
                    match stream.read(&mut buf) {
                        Ok(0) => capture_stream = None,
                        Ok(size) => frames.extend_from_slice(&buf[..size]),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => capture_stream = None,
                    }
                }
                while let Some(frame) = super::take_qemu_filter_frame(&mut frames) {
                    let Some(request) = super::parse_discovery_frame(&frame) else {
                        continue;
                    };
                    println!("axloader OTA mock: captured v4 discovery");
                    let offer = serde_json::json!({
                        "protocol_version": 4, "server_id": "qemu-ota-mock",
                        "control_base_url": format!("http://10.0.2.2:{port}"),
                        "registration_id": ID, "expires_in_ms": 60000,
                    });
                    let reply =
                        super::build_discovery_reply(&request, offer.to_string().as_bytes());
                    thread::sleep(super::HTTP_SMOKE_DISCOVERY_REPLY_DELAY);
                    for _ in 0..10 {
                        if udp_polled.load(Ordering::Acquire) {
                            break;
                        }
                        let _ = udp_forward
                            .send_to(offer.to_string().as_bytes(), ("127.0.0.1", udp_hostfwd));
                        thread::sleep(Duration::from_millis(500));
                    }
                    println!("axloader OTA mock: forwarded v4 offer through QEMU UDP");
                    for _ in 0..100 {
                        if inject_stream.is_some() {
                            break;
                        }
                        super::accept_qemu_filter(&injection_listener, &mut inject_stream);
                        thread::sleep(Duration::from_millis(10));
                    }
                    if let Some(stream) = inject_stream.as_mut() {
                        if super::write_qemu_filter_frame(stream, &reply).is_err() {
                            inject_stream = None;
                            println!("axloader OTA mock: v4 offer injection failed");
                        } else {
                            println!("axloader OTA mock: injected v4 offer");
                        }
                    } else {
                        println!("axloader OTA mock: injection chardev not connected");
                    }
                }
                thread::sleep(Duration::from_millis(10));
            }
        });
        Ok(Self {
            stop,
            confirmed,
            threads: vec![http_thread, udp_thread],
            capture,
            inject,
            udp_hostfwd,
        })
    }

    fn filter_ports(&self) -> (u16, u16, u16) {
        (self.capture, self.inject, self.udp_hostfwd)
    }
}

impl Drop for AssignedOtaServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
