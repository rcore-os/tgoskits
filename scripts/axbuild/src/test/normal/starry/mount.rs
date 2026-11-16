//! Boot-time mount permission checks using the grouped suite's compiled init.

use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::Path,
    process::Command,
};

use anyhow::{Context, ensure};
use ostool::run::{ShellCheckStep, qemu::QemuConfig};
use serde::Deserialize;

/// Owns every temporary image until both root permission checks have finished.
pub(super) struct FatRootFixture {
    /// Root and executable images, removed together on success or failure.
    directory: tempfile::TempDir,
}

#[derive(Deserialize)]
struct MountConfig {
    /// Existing grouped executable to reuse as PID 1 on a readonly FAT root.
    fat_root_init: Option<String>,
}

impl FatRootFixture {
    /// Extracts the already compiled executable without changing the source or
    /// cached root image. FAT's executable-mode policy requires an ext4 payload.
    pub(super) fn prepare(config: &Path, rootfs: &Path) -> anyhow::Result<Option<Self>> {
        let config: MountConfig = toml::from_str(&fs::read_to_string(config)?)?;
        let Some(init) = config.fat_root_init else {
            return Ok(None);
        };
        ensure!(
            init.starts_with('/')
                && init
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"/_-.".contains(&byte)),
            "fat_root_init must be an absolute guest executable path"
        );
        let directory = tempfile::tempdir()?;
        let stage = directory.path().join("payload");
        fs::create_dir(&stage)?;
        let executable = stage.join("init");
        checked(
            Command::new("debugfs")
                .arg("-R")
                .arg(format!("dump {init} {}", executable.display()))
                .arg(rootfs),
        )?;
        ensure!(executable.is_file(), "debugfs did not extract {init}");

        let mut fat = image(&directory.path().join("root.img"), 64 * 1024 * 1024)?;
        fatfs::format_volume(
            &mut fat,
            fatfs::FormatVolumeOptions::new().fat_type(fatfs::FatType::Fat16),
        )?;
        fat.rewind()?;
        let filesystem = fatfs::FileSystem::new(fat, fatfs::FsOptions::new())?;
        for name in ["dev", "tmp", "proc", "sys", "partition"] {
            filesystem.root_dir().create_dir(name)?;
        }
        filesystem.unmount()?;

        let ext4 = directory.path().join("payload.ext4");
        image(&ext4, 32 * 1024 * 1024)?;
        checked(
            Command::new("mkfs.ext4")
                .args(["-q", "-F", "-d"])
                .arg(&stage)
                .arg(&ext4),
        )?;
        checked(
            Command::new("debugfs")
                .args(["-w", "-R", "set_inode_field /init mode 0100755"])
                .arg(&ext4),
        )?;
        let mut disk = image(&directory.path().join("payload.img"), 34 * 1024 * 1024)?;
        // One Linux MBR partition is enough for the existing additional-volume
        // mount path (/partition). No partition-tool subprocess is needed.
        let mut mbr = [0u8; 512];
        mbr[450] = 0x83;
        mbr[454..458].copy_from_slice(&2048u32.to_le_bytes());
        mbr[458..462].copy_from_slice(&65536u32.to_le_bytes());
        mbr[510..512].copy_from_slice(&[0x55, 0xaa]);
        disk.write_all(&mbr)?;
        disk.seek(SeekFrom::Start(1024 * 1024))?;
        std::io::copy(&mut File::open(ext4)?, &mut disk)?;
        disk.sync_all()?;
        Ok(Some(Self { directory }))
    }

    /// Reuses platform and boot payload settings while replacing only disks
    /// and the expected mount result. Both runs discard all guest writes.
    pub(super) fn qemu(&self, template: &QemuConfig, readonly: bool) -> QemuConfig {
        let mut qemu = template.clone();
        qemu.args.clear();
        let mut args = template.args.iter();
        while let Some(option) = args.next() {
            if option == "-drive" {
                args.next();
            } else if option == "-device" {
                if let Some(device) = args.next()
                    && !device.starts_with("nvme,")
                    && !device.starts_with("virtio-blk-")
                {
                    qemu.args.extend([option.clone(), device.clone()]);
                }
            } else {
                qemu.args.push(option.clone());
            }
        }
        qemu.args.extend([
            "-device".into(),
            "virtio-blk-device,drive=payload".into(),
            "-drive".into(),
            format!(
                "id=payload,if=none,format=raw,snapshot=on,file={}",
                self.directory.path().join("payload.img").display()
            ),
            "-device".into(),
            "virtio-blk-device,drive=root".into(),
            "-drive".into(),
            format!(
                "id=root,if=none,format=raw,snapshot=on,readonly={},file={}",
                if readonly { "on" } else { "off" },
                self.directory.path().join("root.img").display()
            ),
        ]);
        let (mode, marker) = if readonly {
            ("--physical-readonly-root", "PHYSICAL_READONLY_ROOT_PASSED")
        } else {
            ("--readonly-root", "READONLY_ROOT_PROMOTION_PASSED")
        };
        qemu.boot.cmdline = Some(format!("root=/dev/vda ro init=/partition/init -- {mode}"));
        qemu.shell_check_steps = vec![ShellCheckStep {
            success_regex: Some(vec![format!("(?m)^{marker}\\s*$")]),
            ..Default::default()
        }];
        qemu.fail_regex.push("FAIL:".into());
        qemu
    }
}

/// Creates a fresh, sparse writable fixture image.
fn image(path: &Path, bytes: u64) -> anyhow::Result<File> {
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)?;
    file.set_len(bytes)?;
    Ok(file)
}

/// Preserves subprocess failures and diagnostics at the suite boundary.
fn checked(command: &mut Command) -> anyhow::Result<()> {
    let output = command
        .output()
        .with_context(|| format!("failed to run {command:?}"))?;
    ensure!(
        output.status.success(),
        "{command:?}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
