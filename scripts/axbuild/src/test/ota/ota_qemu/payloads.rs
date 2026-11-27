//! Boot the actual StarryOS userspace and an AxVisor Linux guest through HTTP.

use std::{
    fs,
    io::Write,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{Context, bail, ensure};

use super::{
    BootScenario, QemuChild, QemuProfile, STATUS_TIMEOUT, boot_smoke, start_qemu_with_resources,
    status,
};
use crate::{
    context::{AXVISOR_SNAPSHOT_FILE, STARRY_SNAPSHOT_FILE, WorkspaceContext, axbuild_tmp_dir},
    image::{config::ImageConfig, spec::ImageSpecRef, storage::Storage},
    support::ovmf::OvmfFirmware,
};

pub(super) struct OsPayloads {
    starry: Vec<u8>,
    userspace: Vec<u8>,
    axvisor: Vec<u8>,
    guests: Vec<u8>,
}

impl OsPayloads {
    pub(super) async fn build(workspace: &WorkspaceContext) -> anyhow::Result<Self> {
        let snapshot_paths = [STARRY_SNAPSHOT_FILE, AXVISOR_SNAPSHOT_FILE]
            .map(|name| axbuild_tmp_dir(workspace.root()).join(name));
        let snapshots = snapshot_paths
            .iter()
            .map(|path| read_snapshot(path))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let fixtures = workspace.root().join("bootloader/axloader/test-payloads");
        let payloads = workspace.axbuild_artifact_dir().join("axloader/payloads");
        fs::create_dir_all(&payloads)?;
        let mut config = ImageConfig::read_config(workspace.root(), workspace.target_dir())?;
        config.extract_dir = workspace.axbuild_artifact_dir().join("images");
        let storage = Storage::new_from_config(&config).await?;
        let busybox = storage
            .pull_image(
                ImageSpecRef::parse("initramfs-x86_64-busybox.cpio.gz"),
                true,
            )
            .await?
            .join("initramfs-x86_64-busybox.cpio.gz");
        let userspace_path = payloads.join("userspace.cpio");
        let overlay = payloads.join("smoke.cpio");
        crate::image::pack_initramfs_dir(&fixtures.join("root"), &overlay)?;
        fs::copy(busybox, &userspace_path)?;
        let mut output = fs::OpenOptions::new().append(true).open(&userspace_path)?;
        // A gzip member may end unaligned; the following raw newc stream must not.
        let padding = (4 - output.metadata()?.len() % 4) % 4;
        output.write_all(&[0; 3][..padding as usize])?;
        std::io::copy(&mut fs::File::open(overlay)?, &mut output)?;
        drop(output);
        let userspace = fs::read(userspace_path)?;

        build_os(
            workspace,
            "starry",
            &fixtures.join("build-starry-x86_64.toml"),
        )
        .await?;
        let starry = fs::read(
            workspace
                .target_dir()
                .join("x86_64-unknown-none/release/starryos"),
        )
        .context("read StarryOS EFI-entry ELF")?;

        let axvisor_output = build_os(
            workspace,
            "axvisor",
            &fixtures.join("build-axvisor-x86_64.toml"),
        )
        .await?;
        let axvisor = fs::read(
            workspace
                .target_dir()
                .join("x86_64-unknown-linux-musl/release/axvisor"),
        )
        .context("read AxVisor EFI-entry ELF")?;
        let guests = read_published_guest_bundle(&axvisor_output)?;
        for (path, before) in snapshot_paths.iter().zip(snapshots) {
            ensure!(
                read_snapshot(path)? == before,
                "test build changed saved OS command snapshot {}",
                path.display()
            );
        }
        println!("axloader QEMU: saved OS command snapshots preserved");
        Ok(Self {
            starry,
            userspace,
            axvisor,
            guests,
        })
    }

    pub(super) async fn test(
        &self,
        profile: QemuProfile,
        client: &reqwest::Client,
        firmware: &OvmfFirmware,
        root: &Path,
        port: u16,
    ) -> anyhow::Result<()> {
        for (memory, cpus, guest_console, scenario) in [
            (
                "512M",
                "1",
                None,
                BootScenario {
                    name: "starry-userspace",
                    kernel: &self.starry,
                    initramfs: Some(&self.userspace),
                    cmdline: Some("rdinit=/bin/sh -- /axloader-smoke.sh x86-handoff STARRY"),
                    expected_lines: &["STARRY_AXLOADER_USERSPACE_PASSED"],
                    check_retries: false,
                },
            ),
            (
                "2G",
                "2",
                Some(1),
                BootScenario {
                    name: "axvisor-linux-guest",
                    kernel: &self.axvisor,
                    initramfs: Some(&self.guests),
                    cmdline: Some("axvisor.vm_start=1"),
                    expected_lines: &["AXVISOR_GUEST_AXLOADER_USERSPACE_PASSED"],
                    check_retries: false,
                },
            ),
        ] {
            let qemu = start_qemu_with_resources(profile, firmware, root, port, memory, cpus)?;
            status(&qemu, client, port, root, |value| {
                value["running_slot"] == "b"
            })
            .await?;
            if let Some(vm_id) = guest_console {
                tokio::try_join!(
                    boot_smoke(&qemu, client, port, root, scenario),
                    attach_guest_console(&qemu, root, vm_id),
                )?;
            } else {
                boot_smoke(&qemu, client, port, root, scenario).await?;
            }
            drop(qemu);
        }
        Ok(())
    }
}

async fn build_os(workspace: &WorkspaceContext, os: &str, config: &Path) -> anyhow::Result<String> {
    // Keep the shared OS build pipeline while isolating its command history
    // policy in a child environment; the parent Tokio runtime may be threaded.
    let output = tokio::process::Command::new("cargo")
        .args(["xtask", os, "build", "--config"])
        .arg(config)
        .current_dir(workspace.root())
        .env("CARGO_TARGET_DIR", workspace.target_dir())
        .env("AXBUILD_NO_SNAPSHOT", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("build {os} payload using {}", config.display()))?;
    let stdout =
        String::from_utf8(output.stdout).context("OS payload build output is not UTF-8")?;
    print!("{stdout}");
    ensure!(
        output.status.success(),
        "{os} payload build failed: {}",
        output.status
    );
    Ok(stdout)
}

fn read_published_guest_bundle(stdout: &str) -> anyhow::Result<Vec<u8>> {
    use sha2::{Digest, Sha256};

    use crate::axvisor::bundle::{BUNDLE_REPORT_PREFIX, GuestBundleArtifact};
    let reports = stdout
        .lines()
        .filter_map(|line| line.strip_prefix(BUNDLE_REPORT_PREFIX))
        .collect::<Vec<_>>();
    ensure!(
        reports.len() == 1,
        "AxVisor build must publish exactly one guest archive"
    );
    let artifact: GuestBundleArtifact = serde_json::from_str(reports[0])?;
    let bytes = fs::read(&artifact.path).context("read published AxVisor guest archive")?;
    ensure!(
        format!("{:x}", Sha256::digest(&bytes)) == artifact.sha256,
        "published AxVisor guest archive checksum mismatch"
    );
    Ok(bytes)
}

fn read_snapshot(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("read OS command snapshot {}", path.display()))
        }
    }
}

async fn attach_guest_console(qemu: &QemuChild, root: &Path, vm_id: u32) -> anyhow::Result<()> {
    let deadline = Instant::now() + STATUS_TIMEOUT;
    while Instant::now() < deadline {
        let log = fs::read_to_string(root.join("ota-qemu.log")).unwrap_or_default();
        if log.contains("axvisor:/$") {
            let mut child = qemu.process.lock().expect("QEMU process lock poisoned");
            let input = child
                .stdin
                .as_mut()
                .context("QEMU serial input is unavailable")?;
            writeln!(input, "vm console {vm_id}")?;
            input.flush()?;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!(
        "AxVisor shell did not become available for guest console attachment: {}",
        fs::read_to_string(root.join("ota-qemu.log")).unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::axvisor::bundle::{BUNDLE_REPORT_PREFIX, GuestBundleArtifact};

    #[test]
    fn boot_payload_uses_the_exact_reported_archive_and_validates_its_contents() {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("guest bundle with spaces.cpio");
        fs::write(&archive, b"selected guest catalog").unwrap();
        let record = serde_json::to_string(&GuestBundleArtifact {
            path: archive.clone(),
            sha256: format!("{:x}", Sha256::digest(b"selected guest catalog")),
        })
        .unwrap();
        let output = format!("ordinary build output\n{BUNDLE_REPORT_PREFIX}{record}\n");
        assert_eq!(
            read_published_guest_bundle(&output).unwrap(),
            b"selected guest catalog"
        );
        assert!(read_published_guest_bundle("no report").is_err());
        assert!(read_published_guest_bundle(&output.repeat(2)).is_err());
        fs::write(archive, b"a different catalog").unwrap();
        assert!(read_published_guest_bundle(&output).is_err());
    }
}
