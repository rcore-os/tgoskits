//! Persistent FAT image OTA checks using real OVMF TCP4 and host forwarding.

use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, bail, ensure};
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions, format_volume};
use ostool::ovmf::Arch;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    build::BuildInfo,
    context::{AppContext, WorkspaceContext},
    support::ovmf::OvmfFirmware,
};

const STATUS_TIMEOUT: Duration = Duration::from_secs(75);

struct QemuChild(Child);

impl Drop for QemuChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_qemu(firmware: &OvmfFirmware, root: &Path, port: u16) -> anyhow::Result<QemuChild> {
    let serial = fs::File::create(root.join("ota-qemu.log"))?;
    let vars = root.join("vars.fd");
    let disk = root.join("esp.img");
    let mut command = Command::new("qemu-system-x86_64");
    let netdev = format!("user,id=user0,hostfwd=tcp:127.0.0.1:{port}-:2999");
    command.args([
        "-m", "256M", "-smp", "1", "-machine", "q35", "-accel", "kvm", "-cpu", "host", "-display",
        "none", "-monitor", "none", "-serial", "stdio", "-netdev", &netdev,
    ]);
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

async fn epoch(client: &reqwest::Client, port: u16) -> anyhow::Result<String> {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let response = client
            .get(format!("http://127.0.0.1:{port}/api/v1/status"))
            .timeout(Duration::from_secs(6))
            .send()
            .await;
        if let Ok(response) = response
            && let Ok(status) = response.error_for_status()
            && let Ok(value) = status.json::<Value>().await
            && let Some(epoch) = value["boot_epoch"].as_str()
        {
            return Ok(epoch.to_owned());
        }
        ensure!(
            Instant::now() < deadline,
            "loader epoch did not become available"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn upload(
    client: &reqwest::Client,
    port: u16,
    bytes: &[u8],
    digest: &str,
) -> anyhow::Result<reqwest::Response> {
    client
        .put(format!("http://127.0.0.1:{port}/api/v1/ota/image"))
        .header("X-Boot-Epoch", epoch(client, port).await?)
        .header("X-Image-Sha256", digest)
        .body(bytes.to_vec())
        .send()
        .await
        .context("direct OTA upload failed")
}

struct BootKernels {
    cmdline: Vec<u8>,
    initramfs: Vec<u8>,
}

struct BootScenario<'a> {
    name: &'a str,
    kernel: &'a [u8],
    initramfs: Option<&'a [u8]>,
    cmdline: Option<&'a str>,
    expected_lines: &'a [&'a str],
    check_retries: bool,
}

async fn build_boot_kernels(workspace: &WorkspaceContext) -> anyhow::Result<BootKernels> {
    let mut app = AppContext::from_workspace_root(workspace.root(), Some(workspace.target_dir()))?;
    let build_config = workspace.root().join("Cargo.toml");
    let build = |feature: &str| {
        let mut info = BuildInfo::default().with_features([feature]);
        info.max_cpu_num = Some(1);
        info.into_prepared_std_cargo_config_with_metadata(
            "arceos-helloworld",
            "x86_64-unknown-none",
            workspace.metadata(),
            &workspace.axbuild_artifact_dir(),
        )
    };
    let output = app
        .build(build("cmdline-smoke")?, build_config.clone())
        .await?;
    let cmdline = fs::read(output.elf_path()).context("failed to read cmdline smoke kernel")?;
    let output = app.build(build("initramfs-smoke")?, build_config).await?;
    let initramfs = fs::read(output.elf_path()).context("failed to read initramfs smoke kernel")?;
    Ok(BootKernels { cmdline, initramfs })
}

pub(super) async fn test_direct_ota(
    workspace: &WorkspaceContext,
    target: &str,
    server_only: bool,
) -> anyhow::Result<()> {
    if target != super::DEFAULT_UEFI_TARGET {
        bail!("persistent OTA QEMU test supports only x86_64 UEFI");
    }
    let kernels = if server_only {
        None
    } else {
        Some(build_boot_kernels(workspace).await?)
    };
    let workspace_root = workspace.root();
    let target_dir = workspace.target_dir();
    let root = tempfile::tempdir().context("failed to create OTA FAT image directory")?;
    let root = root.path();
    let host_initramfs_path = root.join("host-initramfs.cpio");
    if !server_only {
        crate::image::pack_initramfs_dir(
            &workspace_root.join("test-suit/host-initramfs"),
            &host_initramfs_path,
        )?;
    }
    let host_initramfs = if server_only {
        None
    } else {
        Some(fs::read(&host_initramfs_path).context("failed to read host initramfs")?)
    };
    let firmware = OvmfFirmware::fetch(Arch::X64).await?;
    fs::copy(firmware.vars(), root.join("vars.fd"))?;
    let loader = fs::read(target_dir.join(target).join("release/axloader.efi"))?;
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
        .arg(workspace_root.join("bootloader/axloader/scripts/init-ota-state.py"))
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
    create_esp_image(root, &disk)?;
    let port = TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(35))
        .build()?;
    if server_only {
        let qemu = start_qemu(&firmware, root, port)?;
        status(&client, port, root, |value| value["running_slot"] == "a").await?;
        server_assignment(&client, port, root, &loader).await?;
        drop(qemu);
        println!("axloader OTA QEMU: assigned v5 upload and confirmation passed");
        return Ok(());
    }
    let mut qemu = start_qemu(&firmware, root, port)?;
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
        .header("X-Boot-Epoch", epoch(&client, port).await?)
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

    let kernels = kernels.context("boot smoke kernels were not built")?;
    let host_initramfs = host_initramfs.context("host initramfs was not packed")?;
    let scenarios = [
        BootScenario {
            name: "no-options",
            kernel: &kernels.cmdline,
            initramfs: None,
            cmdline: None,
            expected_lines: &["HOST_CMDLINE: ", "Hello, world!"],
            check_retries: false,
        },
        BootScenario {
            name: "cmdline-only",
            kernel: &kernels.cmdline,
            initramfs: None,
            cmdline: Some("axloader.cmdline=qemu-direct"),
            expected_lines: &[
                "HOST_CMDLINE: axloader.cmdline=qemu-direct",
                "Hello, world!",
            ],
            check_retries: false,
        },
        BootScenario {
            name: "initramfs-only",
            kernel: &kernels.initramfs,
            initramfs: Some(&host_initramfs),
            cmdline: None,
            expected_lines: &["HOST_CMDLINE: ", "HOST_INITRAMFS_PASSED", "Hello, world!"],
            check_retries: false,
        },
        BootScenario {
            name: "both",
            kernel: &kernels.initramfs,
            initramfs: Some(&host_initramfs),
            cmdline: Some("axloader.cmdline=qemu-direct"),
            expected_lines: &[
                "HOST_CMDLINE: axloader.cmdline=qemu-direct",
                "HOST_INITRAMFS_PASSED",
                "Hello, world!",
            ],
            check_retries: true,
        },
    ];
    for scenario in scenarios {
        status(&client, port, root, |value| value["running_slot"] == "b").await?;
        boot_smoke(&client, port, root, scenario).await?;
        drop(qemu);
        qemu = start_qemu(&firmware, root, port)?;
    }
    status(&client, port, root, |value| value["running_slot"] == "b").await?;
    println!("axloader OTA QEMU: checking failed uploads on stable B");

    let digest = format!("{:x}", Sha256::digest(&old_loader));
    let wrong = upload(&client, port, &old_loader, &"0".repeat(64)).await?;
    ensure!(
        wrong.status() == reqwest::StatusCode::UNPROCESSABLE_ENTITY,
        "bad image SHA was accepted"
    );
    let mut short = TcpStream::connect(format!("127.0.0.1:{port}"))?;
    short.write_all(
        format!(
            "PUT /api/v1/ota/image HTTP/1.1\r\nHost: localhost\r\nX-Boot-Epoch: \
             {}\r\nContent-Length: 10000\r\nX-Image-Sha256: {}\r\n\r\nhello",
            epoch(&client, port).await?,
            "0".repeat(64)
        )
        .as_bytes(),
    )?;
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
    qemu = start_qemu(&firmware, root, port)?;
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
        .header("X-Boot-Epoch", epoch(&client, port).await?)
        .json(&serde_json::json!({"update_id": id}))
        .send()
        .await?;
    ensure!(
        confirmed.status().is_success(),
        "second confirmation failed"
    );
    drop(qemu);
    let qemu = start_qemu(&firmware, root, port)?;
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
    write_esp_file(&disk, "EFI/AXLOADER/B.EFI", &uncommitted)?;
    let qemu = start_qemu(&firmware, root, port)?;
    status(&client, port, root, |value| {
        value["running_slot"] == "a"
            && value["active_slot"] == "a"
            && value["pending_update_id"].is_null()
    })
    .await?;
    drop(qemu);
    // A valid pending record pointing at a file which cannot be loaded must
    // be committed as a failed trial before returning to the stable slot.
    stage_unloadable_trial(root, &disk)?;
    let qemu = start_qemu(&firmware, root, port)?;
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
    let qemu = start_qemu(&firmware, root, port)?;
    status(&client, port, root, |value| value["running_slot"] == "a").await?;
    server_assignment(&client, port, root, &loader).await?;
    drop(qemu);
    println!("axloader OTA QEMU: assigned v5 confirmation and subsequent FAT boot passed");
    Ok(())
}

fn create_esp_image(root: &Path, disk_path: &Path) -> anyhow::Result<()> {
    const ESP_SIZE: u64 = 128 * 1024 * 1024;
    let mut disk = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(disk_path)
        .context("failed to create ESP image")?;
    disk.set_len(ESP_SIZE)?;
    disk.seek(SeekFrom::Start(0))?;
    format_volume(
        &mut disk,
        FormatVolumeOptions::new()
            .fat_type(FatType::Fat32)
            .volume_label(*b"OSTOOLBOOT "),
    )
    .context("failed to format ESP image")?;
    disk.seek(SeekFrom::Start(0))?;
    let fs = FileSystem::new(&mut disk, FsOptions::new()).context("failed to open ESP image")?;
    {
        let root_dir = fs.root_dir();
        root_dir.create_dir("EFI")?;
        root_dir.create_dir("EFI/BOOT")?;
        root_dir.create_dir("EFI/AXLOADER")?;
        copy_into_fat(
            &root_dir,
            "EFI/BOOT/BOOTX64.EFI",
            &fs::read(root.join("BOOTX64.EFI"))?,
        )?;
        for name in ["A.EFI", "B.EFI", "STATE0.BIN", "STATE1.BIN"] {
            copy_into_fat(
                &root_dir,
                &format!("EFI/AXLOADER/{name}"),
                &fs::read(root.join(name))?,
            )?;
        }
    }
    fs.unmount().context("failed to unmount ESP image")?;
    disk.sync_all().context("failed to sync ESP image")
}

fn copy_into_fat<T: fatfs::ReadWriteSeek>(
    root: &fatfs::Dir<'_, T>,
    path: &str,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let mut file = root.create_file(path)?;
    file.truncate()?;
    file.write_all(bytes)?;
    file.flush()?;
    Ok(())
}

fn write_esp_file(disk_path: &Path, path: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let mut disk = OpenOptions::new().read(true).write(true).open(disk_path)?;
    let fs = FileSystem::new(&mut disk, FsOptions::new()).context("failed to open ESP image")?;
    {
        let root = fs.root_dir();
        copy_into_fat(&root, path, bytes)?;
    }
    fs.unmount().context("failed to unmount ESP image")?;
    disk.sync_all().context("failed to sync ESP image")
}

fn read_esp_file(disk_path: &Path, path: &str) -> anyhow::Result<Vec<u8>> {
    let mut disk = OpenOptions::new().read(true).write(true).open(disk_path)?;
    let fs = FileSystem::new(&mut disk, FsOptions::new()).context("failed to open ESP image")?;
    let mut bytes = Vec::new();
    {
        let root = fs.root_dir();
        root.open_file(path)?.read_to_end(&mut bytes)?;
    }
    fs.unmount().context("failed to unmount ESP image")?;
    Ok(bytes)
}

fn stage_unloadable_trial(root: &Path, disk_path: &Path) -> anyhow::Result<()> {
    let invalid = vec![0x55; 4096];
    write_esp_file(disk_path, "EFI/AXLOADER/B.EFI", &invalid)?;
    let records = [root.join("STATE0.BIN"), root.join("STATE1.BIN")];
    for (index, path) in records.iter().enumerate() {
        fs::write(
            path,
            read_esp_file(disk_path, &format!("EFI/AXLOADER/STATE{index}.BIN"))?,
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
    write_esp_file(
        disk_path,
        &format!("EFI/AXLOADER/STATE{target}.BIN"),
        &fs::read(staged)?,
    )
}

async fn server_assignment(
    client: &reqwest::Client,
    port: u16,
    root: &Path,
    image: &[u8],
) -> anyhow::Result<()> {
    const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
    let digest = format!("{:x}", Sha256::digest(image));
    let response = client
        .put(format!("http://127.0.0.1:{port}/api/v1/ota/image"))
        .header("X-Boot-Epoch", epoch(client, port).await?)
        .header("X-Image-Sha256", digest.clone())
        .header("X-Update-Source", "server")
        .header("X-Update-Id", ID)
        .body(image.to_vec())
        .send()
        .await?;
    ensure!(
        response.status() == reqwest::StatusCode::ACCEPTED,
        "assigned upload failed: {}",
        response.status()
    );
    status(client, port, root, |value| {
        value["pending_update_id"] == ID && value["running_slot"] == "b"
    })
    .await?;
    let response = client
        .post(format!("http://127.0.0.1:{port}/api/v1/ota/confirm"))
        .header("X-Boot-Epoch", epoch(client, port).await?)
        .header("X-Update-Source", "server")
        .json(&serde_json::json!({"update_id": ID}))
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "assigned confirmation failed: {}",
        response.status()
    );
    status(client, port, root, |value| {
        value["running_slot"] == "b" && value["active_slot"] == "b" && value["source"] == "server"
    })
    .await?;
    Ok(())
}

async fn boot_smoke(
    client: &reqwest::Client,
    port: u16,
    root: &Path,
    scenario: BootScenario<'_>,
) -> anyhow::Result<()> {
    let kernel = scenario.kernel;
    let base = format!("http://127.0.0.1:{port}/api/v1/boot/jobs");
    let epoch = epoch(client, port).await?;
    let boot_id = format!("qemu-v5-{}", scenario.name);
    let mut manifest = serde_json::json!({
        "boot_id": boot_id,
        "arch": "x86_64",
        "image_format": "elf64",
        "kernel": {"size": kernel.len(), "sha256": format!("{:x}", Sha256::digest(kernel))},
        "entry_symbol": "__x86_64_efi_pe_entry",
    });
    if let Some(initramfs) = scenario.initramfs {
        manifest["initramfs"] = serde_json::json!({
            "size": initramfs.len(),
            "sha256": format!("{:x}", Sha256::digest(initramfs)),
        });
    }
    if let Some(cmdline) = scenario.cmdline {
        manifest["cmdline"] = serde_json::json!(cmdline);
    }
    let response = client
        .post(&base)
        .header("X-Boot-Epoch", &epoch)
        .json(&manifest)
        .send()
        .await?;
    ensure!(
        response.status() == reqwest::StatusCode::CREATED,
        "boot manifest rejected: {}",
        response.text().await?
    );
    if scenario.check_retries {
        let stale = client
            .post(&base)
            .header("X-Boot-Epoch", "previous-generation")
            .json(&manifest)
            .send()
            .await?;
        ensure!(
            stale.status() == reqwest::StatusCode::CONFLICT,
            "stale epoch accepted"
        );
        let repeated = client
            .post(&base)
            .header("X-Boot-Epoch", &epoch)
            .json(&manifest)
            .send()
            .await?;
        ensure!(
            repeated.status() == reqwest::StatusCode::OK,
            "identical boot rejected"
        );
        let concurrent_ota = client
            .put(format!("http://127.0.0.1:{port}/api/v1/ota/image"))
            .header("X-Boot-Epoch", &epoch)
            .header("X-Image-Sha256", "00".repeat(32))
            .body(vec![1])
            .send()
            .await?;
        ensure!(
            concurrent_ota.status() == reqwest::StatusCode::CONFLICT,
            "concurrent OTA accepted"
        );
        let mut conflicting_manifest = manifest.clone();
        conflicting_manifest["boot_id"] = serde_json::json!("another-boot-id");
        let conflicting = client
            .post(&base)
            .header("X-Boot-Epoch", &epoch)
            .json(&conflicting_manifest)
            .send()
            .await?;
        ensure!(
            conflicting.status() == reqwest::StatusCode::CONFLICT,
            "concurrent boot accepted"
        );
        let rejected = client
            .put(format!("{base}/{boot_id}/kernel"))
            .header("X-Boot-Epoch", &epoch)
            .header("X-Image-Sha256", "00".repeat(32))
            .body(kernel.to_vec())
            .send()
            .await?;
        ensure!(
            rejected.status() == reqwest::StatusCode::BAD_REQUEST,
            "bad digest accepted"
        );
        let retry = client
            .post(&base)
            .header("X-Boot-Epoch", &epoch)
            .json(&manifest)
            .send()
            .await?;
        ensure!(
            retry.status() == reqwest::StatusCode::CREATED,
            "failed upload kept its transaction"
        );
    }
    let mut uploads = vec![("kernel", kernel)];
    if let Some(initramfs) = scenario.initramfs {
        uploads.push(("initramfs", initramfs));
    }
    for (kind, bytes) in uploads {
        let digest = format!("{:x}", Sha256::digest(bytes));
        let response = client
            .put(format!("{base}/{boot_id}/{kind}"))
            .header("X-Boot-Epoch", &epoch)
            .header("X-Image-Sha256", digest)
            .body(bytes.to_vec())
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "{kind} upload failed: {}",
            response.status()
        );
    }
    let response = client
        .post(format!("{base}/{boot_id}/start"))
        .header("X-Boot-Epoch", &epoch)
        .send()
        .await?;
    ensure!(
        response.status() == reqwest::StatusCode::ACCEPTED,
        "boot handoff refused: {}",
        response.text().await?
    );
    let deadline = Instant::now() + STATUS_TIMEOUT;
    while Instant::now() < deadline {
        let log = fs::read_to_string(root.join("ota-qemu.log")).unwrap_or_default();
        let lines = log
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line));
        if scenario
            .expected_lines
            .iter()
            .all(|expected| lines.clone().any(|line| line == *expected))
        {
            println!("axloader QEMU: {} kernel output passed", scenario.name);
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!(
        "{} kernel output was incomplete: {}",
        scenario.name,
        fs::read_to_string(root.join("ota-qemu.log")).unwrap_or_default()
    )
}
