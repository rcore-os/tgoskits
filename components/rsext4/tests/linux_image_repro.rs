use std::{
    cell::Cell,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    process::{Command, Output},
};

use rsext4::{
    bmalloc::AbsoluteBN,
    error::{Ext4Error, Ext4Result},
    *,
};

struct FileBlockDevice {
    file: File,
    block_size: u32,
    total_blocks: u64,
    now: Cell<i64>,
}

impl FileBlockDevice {
    fn open(path: PathBuf) -> Self {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open image");
        let len = file.metadata().expect("image metadata").len();
        Self {
            file,
            block_size: BLOCK_SIZE as u32,
            total_blocks: len / BLOCK_SIZE as u64,
            now: Cell::new(1_700_000_000),
        }
    }
}

impl BlockDevice for FileBlockDevice {
    fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
        let required = self.block_size as usize * count as usize;
        if buffer.len() < required {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required));
        }
        let start = block_id.raw() * self.block_size as u64;
        self.file
            .seek(SeekFrom::Start(start))
            .map_err(|_| Ext4Error::io())?;
        self.file
            .read_exact(&mut buffer[..required])
            .map_err(|_| Ext4Error::io())
    }

    fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
        let required = self.block_size as usize * count as usize;
        if buffer.len() < required {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required));
        }
        let start = block_id.raw() * self.block_size as u64;
        self.file
            .seek(SeekFrom::Start(start))
            .map_err(|_| Ext4Error::io())?;
        self.file
            .write_all(&buffer[..required])
            .map_err(|_| Ext4Error::io())
    }

    fn open(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn close(&mut self) -> Ext4Result<()> {
        self.flush()
    }

    fn flush(&mut self) -> Ext4Result<()> {
        self.file.sync_all().map_err(|_| Ext4Error::io())
    }

    fn total_blocks(&self) -> u64 {
        self.total_blocks
    }

    fn block_size(&self) -> u32 {
        self.block_size
    }

    fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

fn command_text(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn e2fsck_status_ok(output: &Output, allow_fixed: bool) -> bool {
    match output.status.code() {
        Some(0) => true,
        Some(1) if allow_fixed => true,
        _ => false,
    }
}

fn repair_baseline_image(path: &PathBuf) {
    let probe = Command::new("e2fsck")
        .args(["-fn"])
        .arg(path)
        .output()
        .expect("probe e2fsck");
    let probe_text = command_text(&probe);

    if probe_text.contains("FEATURE_C12") {
        let output = Command::new("debugfs")
            .args(["-w", "-R", "feature ^FEATURE_C12"])
            .arg(path)
            .output()
            .expect("clear unsupported local test feature");
        assert!(
            output.status.success(),
            "debugfs failed while clearing FEATURE_C12\n{}",
            command_text(&output)
        );
    }

    let output = Command::new("e2fsck")
        .args(["-fy"])
        .arg(path)
        .output()
        .expect("repair baseline image");
    assert!(
        e2fsck_status_ok(&output, true),
        "baseline e2fsck repair failed\n{}",
        command_text(&output)
    );
}

#[test]
#[ignore = "requires a Linux-created ext4 rootfs image"]
fn repro_linux_image_create_write_rename_then_e2fsck() {
    let src_from_env = std::env::var_os("RSEXT4_TEST_IMAGE").map(PathBuf::from);
    let src = src_from_env.clone().unwrap_or_else(|| {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest_dir
            .parent()
            .and_then(|p| p.parent())
            .expect("repo root")
            .join("target/rootfs/rootfs-aarch64-debian.img")
    });

    if !src.exists() {
        if src_from_env.is_some() {
            panic!("test image does not exist: {}", src.display());
        }
        eprintln!("skip: default test image does not exist: {}", src.display());
        return;
    }

    let dst = std::env::temp_dir().join(format!(
        "rsext4-linux-image-repro-{}.img",
        std::process::id()
    ));
    fs::copy(&src, &dst).expect("copy test image");
    repair_baseline_image(&dst);

    {
        let dev = FileBlockDevice::open(dst.clone());
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = mount(&mut dev).expect("mount image");

        let probe = "/root/codex-fsck-probe";
        let _ = delete_dir(&mut fs, &mut dev, probe);
        mkdir(&mut dev, &mut fs, &format!("{probe}/sub")).expect("mkdir probe");
        mkfile(
            &mut dev,
            &mut fs,
            &format!("{probe}/sub/data.txt"),
            Some(b"line-0-starry-fsck-probe\n"),
            None,
        )
        .expect("create data");
        write_file(
            &mut dev,
            &mut fs,
            &format!("{probe}/sub/data.txt"),
            25,
            b"tail-starry-fsck-probe\n",
        )
        .expect("append data");
        rename(
            &mut dev,
            &mut fs,
            &format!("{probe}/sub/data.txt"),
            &format!("{probe}/data-renamed.txt"),
        )
        .expect("rename data");
        umount(fs, &mut dev).expect("umount image");
    }

    let output = Command::new("e2fsck")
        .args(["-fn"])
        .arg(&dst)
        .output()
        .expect("run e2fsck");
    assert!(
        e2fsck_status_ok(&output, false),
        "e2fsck failed\n{}",
        command_text(&output)
    );

    let _ = fs::remove_file(dst);
}
