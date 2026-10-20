//! Boot the actual StarryOS userspace and an AxVisor Linux guest through HTTP.

use std::{
    fs,
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};

use super::{
    BootScenario, QemuChild, STATUS_TIMEOUT, boot_smoke, start_qemu_with_resources, status,
};
use crate::{
    axvisor::{self, Axvisor},
    context::WorkspaceContext,
    image::{config::ImageConfig, spec::ImageSpecRef, storage::Storage},
    starry::{self, Starry},
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

        Starry::new()?
            .execute(starry::Command::Build(starry::ArgsBuild {
                config: Some(fixtures.join("build-starry-x86_64.toml")),
                arch: None,
                target: None,
                smp: None,
                debug: false,
            }))
            .await?;
        let starry = fs::read(
            workspace
                .target_dir()
                .join("x86_64-unknown-none/release/starryos"),
        )
        .context("read StarryOS EFI-entry ELF")?;

        Axvisor::new()?
            .execute(axvisor::Command::Build(axvisor::ArgsBuild {
                config: Some(fixtures.join("build-axvisor-x86_64.toml")),
                arch: None,
                target: None,
                smp: None,
                debug: false,
                vmconfigs: Vec::new(),
            }))
            .await?;
        let axvisor = fs::read(
            workspace
                .target_dir()
                .join("x86_64-unknown-linux-musl/release/axvisor"),
        )
        .context("read AxVisor EFI-entry ELF")?;
        let guests = fs::read(
            workspace
                .axbuild_artifact_dir()
                .join("axvisor/host-initramfs/x86_64.cpio"),
        )
        .context("read AxVisor builtin guest archive")?;
        Ok(Self {
            starry,
            userspace,
            axvisor,
            guests,
        })
    }

    pub(super) async fn test(
        &self,
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
            let mut qemu = start_qemu_with_resources(firmware, root, port, memory, cpus)?;
            status(client, port, root, |value| value["running_slot"] == "b").await?;
            if let Some(vm_id) = guest_console {
                tokio::try_join!(
                    boot_smoke(client, port, root, scenario),
                    attach_guest_console(&mut qemu, root, vm_id),
                )?;
            } else {
                boot_smoke(client, port, root, scenario).await?;
            }
            drop(qemu);
        }
        Ok(())
    }
}

async fn attach_guest_console(qemu: &mut QemuChild, root: &Path, vm_id: u32) -> anyhow::Result<()> {
    let deadline = Instant::now() + STATUS_TIMEOUT;
    while Instant::now() < deadline {
        let log = fs::read_to_string(root.join("ota-qemu.log")).unwrap_or_default();
        if log.contains("axvisor:/$") {
            let input = qemu
                .0
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
