//! Persistent FAT image OTA checks using real OVMF TCP4 and host forwarding.

use std::{
    env,
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{Context, bail, ensure};
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions, format_volume};
use ostool::ovmf::Arch;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    build::CommonConfig,
    context::{AppContext, WorkspaceContext},
    support::ovmf::OvmfFirmware,
};

const STATUS_TIMEOUT: Duration = Duration::from_secs(75);

mod payloads;

struct QemuChild(Mutex<Child>);

impl QemuChild {
    fn ensure_running(&self, root: &Path) -> anyhow::Result<()> {
        let mut child = self.0.lock().expect("QEMU process lock poisoned");
        let Some(status) = child.try_wait().context("failed to poll QEMU process")? else {
            return Ok(());
        };
        let serial = fs::read_to_string(root.join("ota-qemu.log")).unwrap_or_default();
        let stderr = fs::read_to_string(root.join("qemu-stderr.log")).unwrap_or_default();
        bail!(
            "QEMU exited before the test completed: {status}; serial tail:\n{}\nstderr tail:\n{}",
            tail(&serial, 4096),
            tail(&stderr, 4096),
        )
    }
}

impl Drop for QemuChild {
    fn drop(&mut self) {
        if let Ok(child) = self.0.get_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn tail(text: &str, limit: usize) -> String {
    text.chars()
        .rev()
        .take(limit)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

#[derive(Clone, Copy)]
pub(super) struct QemuProfile {
    artifact_prefix: &'static str,
    qemu_binary: &'static str,
    machine: &'static str,
    cpu: &'static str,
    accel: &'static str,
    firmware_arch: Arch,
    boot_filename: &'static str,
    manifest_arch: &'static str,
    entry_symbol: &'static str,
    default_memory: &'static str,
    default_cpus: &'static str,
    guest_payloads: bool,
}

impl QemuProfile {
    fn for_target(target: &str) -> anyhow::Result<Self> {
        match target {
            super::DEFAULT_UEFI_TARGET => Ok(Self {
                artifact_prefix: "x86-",
                qemu_binary: "qemu-system-x86_64",
                machine: "q35",
                cpu: "host,-la57",
                accel: "kvm",
                firmware_arch: Arch::X64,
                boot_filename: "BOOTX64.EFI",
                manifest_arch: "x86_64",
                entry_symbol: "__x86_64_efi_pe_entry",
                default_memory: "256M",
                default_cpus: "1",
                guest_payloads: true,
            }),
            super::LOONGARCH64_UEFI_TARGET => Ok(Self {
                artifact_prefix: "loongarch64-",
                qemu_binary: "qemu-system-loongarch64",
                machine: "virt",
                cpu: "la464",
                accel: "tcg",
                firmware_arch: Arch::LoongArch64,
                boot_filename: "BOOTLOONGARCH64.EFI",
                manifest_arch: "loongarch64",
                entry_symbol: "efi_pe_entry",
                default_memory: "1G",
                default_cpus: "1",
                guest_payloads: false,
            }),
            _ => bail!("unsupported axloader UEFI target `{target}`"),
        }
    }

    fn qemu_executable(self) -> PathBuf {
        if matches!(self.firmware_arch, Arch::LoongArch64)
            && let Some(path) = env::var_os("AXBUILD_QEMU_SYSTEM_LOONGARCH64")
                .or_else(|| env::var_os("QEMU_SYSTEM_LOONGARCH64"))
                .map(PathBuf::from)
                .filter(|path| path.is_file())
        {
            return path;
        }
        if matches!(self.firmware_arch, Arch::LoongArch64)
            && Path::new("/opt/qemu-11.1.1/bin/qemu-system-loongarch64").is_file()
        {
            return "/opt/qemu-11.1.1/bin/qemu-system-loongarch64".into();
        }
        self.qemu_binary.into()
    }
}

fn start_qemu(
    profile: QemuProfile,
    firmware: &OvmfFirmware,
    root: &Path,
    port: u16,
) -> anyhow::Result<QemuChild> {
    start_qemu_with_resources(
        profile,
        firmware,
        root,
        port,
        profile.default_memory,
        profile.default_cpus,
    )
}

fn start_qemu_with_resources(
    profile: QemuProfile,
    firmware: &OvmfFirmware,
    root: &Path,
    port: u16,
    memory: &str,
    cpus: &str,
) -> anyhow::Result<QemuChild> {
    // Preserve each completed boot; the live serial path is shared by HTTP waiters.
    if root.join("ota-qemu.log").exists() {
        let index = (1..)
            .find(|index| !root.join(format!("qemu-{index}.log")).exists())
            .context("QEMU log index exhausted")?;
        fs::rename(
            root.join("ota-qemu.log"),
            root.join(format!("qemu-{index}.log")),
        )?;
        fs::rename(
            root.join("qemu-stderr.log"),
            root.join(format!("qemu-{index}.stderr")),
        )?;
    }
    let serial = fs::File::create(root.join("ota-qemu.log"))?;
    let stderr = fs::File::create(root.join("qemu-stderr.log"))?;
    let vars = root.join("vars.fd");
    let disk = root.join("esp.img");
    let mut command = Command::new(profile.qemu_executable());
    let netdev = format!("user,id=user0,hostfwd=tcp:127.0.0.1:{port}-:2999");
    command.args([
        "-m",
        memory,
        "-smp",
        cpus,
        "-machine",
        profile.machine,
        "-accel",
        profile.accel,
        "-cpu",
        profile.cpu,
        "-display",
        "none",
        "-monitor",
        "none",
        "-serial",
        "stdio",
        "-netdev",
        &netdev,
    ]);
    let network_device = if matches!(profile.firmware_arch, Arch::LoongArch64) {
        "virtio-net-pci,netdev=user0,mac=02:00:00:00:00:01,romfile="
    } else {
        "virtio-net-pci,netdev=user0,mac=02:00:00:00:00:01"
    };
    command.args(["-device", network_device]);
    match profile.firmware_arch {
        Arch::LoongArch64 => {
            command.args([
                "-drive",
                &format!(
                    "if=pflash,format=raw,unit=0,readonly=on,file={}",
                    firmware.code().display()
                ),
                "-drive",
                &format!("if=pflash,format=raw,unit=1,file={}", vars.display()),
                "-drive",
                &format!("id=esp,if=none,format=raw,file={}", disk.display()),
                "-device",
                "virtio-blk-pci,drive=esp",
                "-device",
                "virtio-rng-pci,romfile=",
            ]);
        }
        _ => {
            command.args([
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
        }
    }
    let child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::from(serial))
        .stderr(Stdio::from(stderr))
        .spawn()
        .context("failed to start OTA OVMF guest")?;
    Ok(QemuChild(Mutex::new(child)))
}

async fn status(
    qemu: &QemuChild,
    client: &reqwest::Client,
    port: u16,
    root: &Path,
    predicate: impl Fn(&Value) -> bool,
) -> anyhow::Result<Value> {
    let deadline = Instant::now() + STATUS_TIMEOUT;
    let url = format!("http://127.0.0.1:{port}/api/v1/ota/status");
    let mut last = String::new();
    while Instant::now() < deadline {
        qemu.ensure_running(root)?;
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
        tail(&log, 2048)
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

async fn build_boot_kernels(
    workspace: &WorkspaceContext,
    profile: QemuProfile,
) -> anyhow::Result<BootKernels> {
    let mut app = AppContext::from_workspace_root(workspace.root(), Some(workspace.target_dir()))?;
    let build_config = workspace.root().join("Cargo.toml");
    let runtime_env = ax_config::RuntimeEnv::capture()?;
    let (target, cmdline_feature, initramfs_feature) =
        if matches!(profile.firmware_arch, Arch::LoongArch64) {
            (
                "loongarch64-unknown-none-softfloat",
                "loong-efi-cmdline-smoke",
                "loong-efi-initramfs-smoke",
            )
        } else {
            ("x86_64-unknown-none", "cmdline-smoke", "initramfs-smoke")
        };
    let build = |feature: &str| {
        let info = CommonConfig {
            features: vec![feature.to_string()],
            max_cpu_num: Some(1),
            ..CommonConfig::default()
        };
        let runtime = ax_config::resolve_runtime(info.max_cpu_num, info.runtime, &runtime_env)?;
        crate::build::into_prepared_std_cargo_config_with_metadata(
            info,
            "arceos-helloworld",
            target,
            workspace.metadata(),
            &workspace.axbuild_artifact_dir(),
            runtime,
        )
    };
    let output = app
        .build(build(cmdline_feature)?, build_config.clone())
        .await?;
    let cmdline = fs::read(output.elf_path()).with_context(|| {
        format!(
            "failed to read {target} cmdline smoke kernel for {}",
            profile.artifact_prefix
        )
    })?;
    let output = app.build(build(initramfs_feature)?, build_config).await?;
    let initramfs = fs::read(output.elf_path()).with_context(|| {
        format!(
            "failed to read {target} initramfs smoke kernel for {}",
            profile.artifact_prefix
        )
    })?;
    Ok(BootKernels { cmdline, initramfs })
}

pub(super) async fn test_direct_ota(
    workspace: &WorkspaceContext,
    target: &str,
    server_only: bool,
    firmware: Option<OvmfFirmware>,
) -> anyhow::Result<()> {
    let profile = QemuProfile::for_target(target)?;
    let kernels = if server_only {
        None
    } else {
        Some(build_boot_kernels(workspace, profile).await?)
    };
    let os_payloads = if server_only || !profile.guest_payloads {
        None
    } else {
        Some(payloads::OsPayloads::build(workspace).await?)
    };
    let workspace_root = workspace.root();
    let target_dir = workspace.target_dir();
    let artifacts = workspace.axbuild_artifact_dir().join("axloader/qemu");
    fs::create_dir_all(&artifacts)?;
    let root = tempfile::Builder::new()
        .prefix(profile.artifact_prefix)
        .tempdir_in(artifacts)
        .context("failed to create OTA FAT image directory")?
        .keep();
    println!("axloader QEMU artifacts: {}", root.display());
    let root = root.as_path();
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
    let firmware = match firmware {
        Some(firmware) => firmware,
        None => OvmfFirmware::fetch(profile.firmware_arch).await?,
    };
    let firmware = prepare_firmware(profile, &firmware, root)?;
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
        root.join(profile.boot_filename),
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
    create_esp_image(profile, root, &disk)?;
    let port = TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(35))
        .build()?;
    if server_only {
        let qemu = start_qemu(profile, &firmware, root, port)?;
        status(&qemu, &client, port, root, |value| {
            value["running_slot"] == "a"
        })
        .await?;
        server_assignment(&qemu, &client, port, root, &loader).await?;
        drop(qemu);
        println!("axloader OTA QEMU: assigned v6 upload and confirmation passed");
        return Ok(());
    }
    let mut qemu = start_qemu(profile, &firmware, root, port)?;
    let status0 = status(&qemu, &client, port, root, |value| {
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
    status(&qemu, &client, port, root, |value| {
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
        status(&qemu, &client, port, root, |value| {
            value["running_slot"] == "b"
        })
        .await?;
        boot_smoke(profile, &qemu, &client, port, root, scenario).await?;
        drop(qemu);
        qemu = start_qemu(profile, &firmware, root, port)?;
    }
    drop(qemu);
    write_esp_file(&disk, "EFI/BOOT/initramfs.cpio", &host_initramfs)?;
    qemu = start_qemu(profile, &firmware, root, port)?;
    status(&qemu, &client, port, root, |value| {
        value["running_slot"] == "b"
    })
    .await?;
    boot_smoke(
        profile,
        &qemu,
        &client,
        port,
        root,
        BootScenario {
            name: "esp-initramfs-fallback",
            kernel: &kernels.initramfs,
            initramfs: None,
            cmdline: None,
            expected_lines: &["HOST_CMDLINE: ", "HOST_INITRAMFS_PASSED", "Hello, world!"],
            check_retries: false,
        },
    )
    .await?;
    drop(qemu);
    if let Some(payloads) = os_payloads {
        payloads
            .test(profile, &client, &firmware, root, port)
            .await?;
    }
    qemu = start_qemu(profile, &firmware, root, port)?;
    status(&qemu, &client, port, root, |value| {
        value["running_slot"] == "b"
    })
    .await?;
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
    status(&qemu, &client, port, root, |value| {
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
    status(&qemu, &client, port, root, |value| {
        value["trial"] == true && value["running_slot"] == "a" && value["pending_update_id"] == id
    })
    .await?;
    // A killed trial retains its attempted record on the *same* FAT image.
    drop(qemu);
    qemu = start_qemu(profile, &firmware, root, port)?;
    let rolled = status(&qemu, &client, port, root, |value| {
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
    status(&qemu, &client, port, root, |value| {
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
    let qemu = start_qemu(profile, &firmware, root, port)?;
    let stable = status(&qemu, &client, port, root, |value| {
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
    println!("axloader OTA QEMU: checking complete but uncommitted inactive slot");
    fs::write(root.join("UNCOMMITTED.EFI"), &uncommitted)?;
    write_esp_file(&disk, "EFI/AXLOADER/B.EFI", &uncommitted)?;
    let qemu = start_qemu(profile, &firmware, root, port)?;
    status(&qemu, &client, port, root, |value| {
        value["running_slot"] == "a"
            && value["active_slot"] == "a"
            && value["pending_update_id"].is_null()
    })
    .await?;
    drop(qemu);
    // A valid pending record pointing at a file which cannot be loaded must
    // be committed as a failed trial before returning to the stable slot.
    println!("axloader OTA QEMU: checking unloadable pending trial");
    let unloadable_loader = unloadable_slot_image(&loader)?;
    stage_unloadable_trial(root, &disk, &unloadable_loader)?;
    let qemu = start_qemu(profile, &firmware, root, port)?;
    let failed = status(&qemu, &client, port, root, |value| {
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
    println!("axloader OTA QEMU: checking corrupt-header pending trial");
    let corrupt_header = vec![0x55; 4096];
    stage_unloadable_trial(root, &disk, &corrupt_header)?;
    let qemu = start_qemu(profile, &firmware, root, port)?;
    let failed = status(&qemu, &client, port, root, |value| {
        value["running_slot"] == "a"
            && value["active_slot"] == "a"
            && value["pending_update_id"].is_null()
            && value["last_outcome"] == "failed"
    })
    .await?;
    ensure!(
        failed["last_failure_reason"] == "trial_image_load_or_digest_failed",
        "corrupt-header EFI trial did not record the cause: {failed}"
    );
    drop(qemu);
    let qemu = start_qemu(profile, &firmware, root, port)?;
    status(&qemu, &client, port, root, |value| {
        value["running_slot"] == "a"
    })
    .await?;
    server_assignment(&qemu, &client, port, root, &loader).await?;
    drop(qemu);
    println!("axloader OTA QEMU: assigned v6 confirmation and subsequent FAT boot passed");
    Ok(())
}

fn prepare_firmware(
    profile: QemuProfile,
    source: &OvmfFirmware,
    root: &Path,
) -> anyhow::Result<OvmfFirmware> {
    let flash_size = matches!(profile.firmware_arch, Arch::LoongArch64).then_some(16 * 1024 * 1024);
    let code = if flash_size.is_some() {
        let destination = root.join("code.fd");
        fs::copy(source.code(), &destination)?;
        pad_flash(&destination, flash_size)?;
        destination
    } else {
        source.code().to_path_buf()
    };
    let vars = root.join("vars.fd");
    fs::copy(source.vars(), &vars)?;
    pad_flash(&vars, flash_size)?;
    OvmfFirmware::from_paths(code, vars)
}

fn pad_flash(path: &Path, target_size: Option<usize>) -> anyhow::Result<()> {
    let Some(target_size) = target_size else {
        return Ok(());
    };
    let file = OpenOptions::new().write(true).open(path)?;
    if file.metadata()?.len() < target_size as u64 {
        file.set_len(target_size as u64)?;
    }
    Ok(())
}

fn create_esp_image(profile: QemuProfile, root: &Path, disk_path: &Path) -> anyhow::Result<()> {
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
            &format!("EFI/BOOT/{}", profile.boot_filename),
            &fs::read(root.join(profile.boot_filename))?,
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

fn unloadable_slot_image(loader: &[u8]) -> anyhow::Result<Vec<u8>> {
    ensure!(
        loader.len() >= 0x40 && &loader[..2] == b"MZ",
        "invalid loader image"
    );
    let pe_offset =
        usize::try_from(read_le_u32(loader, 0x3c)?).context("loader PE offset overflows")?;
    let end = pe_offset
        .checked_add(184)
        .context("loader PE header range overflows")?;
    ensure!(
        pe_offset.checked_add(4).is_some_and(|start| {
            start <= loader.len() && &loader[pe_offset..start] == b"PE\0\0"
        }) && end <= loader.len(),
        "loader does not contain a complete slot-validation header"
    );
    Ok(loader[..end].to_vec())
}

fn read_le_u32(bytes: &[u8], offset: usize) -> anyhow::Result<u32> {
    let value = bytes
        .get(offset..offset.checked_add(4).context("field offset overflows")?)
        .context("truncated u32 field")?;
    Ok(u32::from_le_bytes(value.try_into().unwrap()))
}

fn stage_unloadable_trial(root: &Path, disk_path: &Path, invalid: &[u8]) -> anyhow::Result<()> {
    write_esp_file(disk_path, "EFI/AXLOADER/B.EFI", invalid)?;
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
    next[52..84].copy_from_slice(&Sha256::digest(invalid));
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
    qemu: &QemuChild,
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
    status(qemu, client, port, root, |value| {
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
    status(qemu, client, port, root, |value| {
        value["running_slot"] == "b" && value["active_slot"] == "b" && value["source"] == "server"
    })
    .await?;
    Ok(())
}

async fn boot_smoke(
    profile: QemuProfile,
    qemu: &QemuChild,
    client: &reqwest::Client,
    port: u16,
    root: &Path,
    scenario: BootScenario<'_>,
) -> anyhow::Result<()> {
    let kernel = scenario.kernel;
    let base = format!("http://127.0.0.1:{port}/api/v1/boot/jobs");
    let epoch = epoch(client, port).await?;
    let boot_id = format!("qemu-v6-{}", scenario.name);
    let mut manifest = serde_json::json!({
        "boot_id": boot_id,
        "arch": profile.manifest_arch,
        "image_format": "elf64",
        "kernel": {"size": kernel.len(), "sha256": format!("{:x}", Sha256::digest(kernel))},
        "entry_symbol": profile.entry_symbol,
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
    // Upload is allowed before binding; handoff requires an exact current token.
    let gated = client
        .post(format!("{base}/{boot_id}/start"))
        .header("X-Boot-Epoch", &epoch)
        .send()
        .await?;
    ensure!(
        !gated.status().is_success(),
        "unconfirmed boot was accepted"
    );
    let observed: serde_json::Value = client
        .get(format!("http://127.0.0.1:{port}/api/v1/status"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let binding_id = "1234567890abcdef1234567890abcdef";
    let bound = scenario.name == "no-options";
    let frame = if bound {
        let id = observed["serial"]["serial_id"]
            .as_str()
            .context("missing UART ID")?;
        format!("AXLOADER-SERIAL/1 {id}")
    } else {
        String::new()
    };
    if bound {
        ensure!(
            observed["serial"]["ready"] == true,
            "firmware UART unavailable: {}",
            observed["serial"]
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let log = fs::read_to_string(root.join("ota-qemu.log")).unwrap_or_default();
            if log.lines().any(|line| line.trim_end_matches('\r') == frame) {
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "UART identity frame did not arrive: {log}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        ensure!(
            observed["serial"]["parameters"]
                == serde_json::json!({
                    "baud_rate": 115_200,
                    "data_bits": 8,
                    "parity": "none",
                    "stop_bits": "one",
                    "flow_control": "none",
                }),
            "unexpected effective UART parameters: {}",
            observed["serial"]["parameters"]
        );
    }
    let binding = serde_json::json!({"serial_id": observed["serial"]["serial_id"], "binding_id": binding_id, "mode": if bound { "bound" } else { "direct" }});
    let continue_url = format!("http://127.0.0.1:{port}/api/v1/serial/continue");
    let malformed = client
        .post(&continue_url)
        .header("X-Boot-Epoch", &epoch)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{")
        .send()
        .await?;
    ensure!(
        malformed.status() == reqwest::StatusCode::BAD_REQUEST,
        "malformed serial binding was not rejected as bad request: {}",
        malformed.status()
    );
    for _ in 0..2 {
        let granted = client
            .post(&continue_url)
            .header("X-Boot-Epoch", &epoch)
            .json(&binding)
            .send()
            .await?;
        ensure!(
            granted.status().is_success(),
            "serial continue failed: {}",
            granted.text().await?
        );
    }
    if bound {
        let frames_before_revoke = fs::read_to_string(root.join("ota-qemu.log"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.trim_end_matches('\r') == frame)
            .count();
        let revoked = client
            .delete(format!(
                "http://127.0.0.1:{port}/api/v1/serial/bindings/{binding_id}"
            ))
            .header("X-Boot-Epoch", &epoch)
            .send()
            .await?;
        ensure!(
            revoked.status() == reqwest::StatusCode::OK,
            "serial binding revoke failed: {}",
            revoked.text().await?
        );
        let status_url = format!("http://127.0.0.1:{port}/api/v1/status");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let value: serde_json::Value = client
                .get(&status_url)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            if value["serial"]["binding"].is_null() {
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "serial binding was not revoked: {value}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let frames = fs::read_to_string(root.join("ota-qemu.log"))
                .unwrap_or_default()
                .lines()
                .filter(|line| line.trim_end_matches('\r') == frame)
                .count();
            if frames > frames_before_revoke {
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "UART identity frame did not resume after revoke"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let regranted = client
            .post(&continue_url)
            .header("X-Boot-Epoch", &epoch)
            .json(&binding)
            .send()
            .await?;
        ensure!(
            regranted.status().is_success(),
            "serial continue after revoke failed: {}",
            regranted.text().await?
        );
    }
    let wrong = client
        .post(format!("{base}/{boot_id}/start"))
        .header("X-Boot-Epoch", &epoch)
        .header("X-Serial-Binding", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .send()
        .await?;
    ensure!(
        !wrong.status().is_success(),
        "conflicting binding token authorized boot"
    );
    let response = client
        .post(format!("{base}/{boot_id}/start"))
        .header("X-Boot-Epoch", &epoch)
        .header("X-Serial-Binding", binding_id)
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
        qemu.ensure_running(root)?;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!(
        "{} kernel output was incomplete: {}",
        scenario.name,
        fs::read_to_string(root.join("ota-qemu.log")).unwrap_or_default()
    )
}
