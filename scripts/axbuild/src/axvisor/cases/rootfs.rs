use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use fatfs::{FileSystem, FsOptions, LossyOemCpConverter};

use crate::axvisor::{cases::RunArtifacts, context::AxvisorContext, qemu_test};

const CASES_TMP_DIR: &str = "tmp/cases";
const EXT4_MAGIC_OFFSET: usize = 1024 + 56;
const EXT4_MAGIC: [u8; 2] = [0x53, 0xEF];
const EXT4_MIN_FREE_BYTES_AFTER_INJECT: u64 = 4 * 1024 * 1024;
const EXT4_MIN_GROW_BYTES: u64 = 64 * 1024 * 1024;

pub(super) async fn prepare_run_artifacts(
    ctx: &AxvisorContext,
    arch: &str,
) -> anyhow::Result<RunArtifacts> {
    let run_id = generate_run_id()?;
    let run_dir = ctx.axvisor_dir().join(CASES_TMP_DIR).join(&run_id);
    fs::create_dir_all(&run_dir)
        .with_context(|| format!("failed to create {}", run_dir.display()))?;

    let base_rootfs = qemu_test::prepare_default_rootfs_for_arch(ctx, arch).await?;
    let target_rootfs = run_dir.join("rootfs.img");
    copy_file(&base_rootfs, &target_rootfs)?;

    Ok(RunArtifacts {
        run_id,
        run_dir: run_dir.clone(),
        target_rootfs,
        summary_path: run_dir.join("summary.json"),
    })
}

fn copy_file(src: &Path, dst: &Path) -> anyhow::Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(src, dst)
        .with_context(|| format!("failed to copy {} to {}", src.display(), dst.display()))?;
    Ok(())
}

pub(super) fn inject_host_file(
    rootfs: &Path,
    guest_path: &str,
    host_path: &Path,
) -> anyhow::Result<()> {
    let bytes = fs::read(host_path)
        .with_context(|| format!("failed to read host file {}", host_path.display()))?;
    inject_bytes(rootfs, guest_path, &bytes)
}

#[cfg(test)]
pub(super) fn inject_text_file(
    rootfs: &Path,
    guest_path: &str,
    contents: &str,
) -> anyhow::Result<()> {
    inject_bytes(rootfs, guest_path, contents.as_bytes())
}

fn generate_run_id() -> anyhow::Result<String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time is before unix epoch")?
        .as_nanos();
    Ok(format!("run-{}-{nanos}", std::process::id()))
}

fn inject_bytes(rootfs: &Path, guest_path: &str, bytes: &[u8]) -> anyhow::Result<()> {
    match detect_rootfs_format(rootfs)? {
        RootfsFormat::Fat => inject_bytes_fat(rootfs, guest_path, bytes),
        RootfsFormat::Ext4 => inject_bytes_ext4(rootfs, guest_path, bytes),
    }
}

fn inject_bytes_fat(rootfs: &Path, guest_path: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(rootfs)
        .with_context(|| format!("failed to open rootfs {}", rootfs.display()))?;
    let fs = FileSystem::new(file, FsOptions::new()).map_err(|err| {
        anyhow::anyhow!(
            "failed to open FAT filesystem {}: {err:?}",
            rootfs.display()
        )
    })?;

    let normalized = normalize_guest_path(guest_path)?;
    let (parent, file_name) = split_guest_path(&normalized)?;
    ensure_guest_dir(&fs, parent)?;

    let root = fs.root_dir();
    let mut file = root
        .create_file(&normalized)
        .map_err(|err| anyhow::anyhow!("failed to create guest file `{normalized}`: {err:?}"))?;
    file.truncate()
        .map_err(|err| anyhow::anyhow!("failed to truncate guest file `{normalized}`: {err:?}"))?;
    file.write_all(bytes)
        .map_err(|err| anyhow::anyhow!("failed to write guest file `{normalized}`: {err:?}"))?;
    file.flush()
        .map_err(|err| anyhow::anyhow!("failed to flush guest file `{normalized}`: {err:?}"))?;
    let _ = file_name;
    Ok(())
}

fn inject_bytes_ext4(rootfs: &Path, guest_path: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let normalized = normalize_guest_path(guest_path)?;
    let guest_abs_path = format!("/{normalized}");
    let (parent, _file_name) = split_guest_path(&normalized)?;
    let temp_file_path = write_temp_file(bytes)?;

    let result = (|| {
        ensure_ext4_capacity(rootfs, bytes.len() as u64)?;
        ensure_guest_dir_ext4(rootfs, parent)?;
        debugfs_write_file(rootfs, &temp_file_path, &guest_abs_path)
    })();

    let _ = fs::remove_file(&temp_file_path);
    result
}

fn detect_rootfs_format(rootfs: &Path) -> anyhow::Result<RootfsFormat> {
    let mut file = fs::File::open(rootfs)
        .with_context(|| format!("failed to open rootfs {}", rootfs.display()))?;
    let mut probe = vec![0u8; EXT4_MAGIC_OFFSET + EXT4_MAGIC.len()];
    let read = file
        .read(&mut probe)
        .with_context(|| format!("failed to probe rootfs {}", rootfs.display()))?;
    if read >= EXT4_MAGIC_OFFSET + EXT4_MAGIC.len()
        && probe[EXT4_MAGIC_OFFSET..EXT4_MAGIC_OFFSET + EXT4_MAGIC.len()] == EXT4_MAGIC
    {
        return Ok(RootfsFormat::Ext4);
    }

    let fat_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(rootfs)
        .with_context(|| format!("failed to reopen rootfs {}", rootfs.display()))?;
    if FileSystem::new(fat_file, FsOptions::new()).is_ok() {
        Ok(RootfsFormat::Fat)
    } else {
        anyhow::bail!(
            "unsupported rootfs format for {}; expected ext4 or FAT",
            rootfs.display()
        );
    }
}

fn write_temp_file(bytes: &[u8]) -> anyhow::Result<std::path::PathBuf> {
    let temp_path = std::env::temp_dir().join(format!(
        "axvisor-cases-{}-{}.tmp",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system time is before unix epoch")?
            .as_nanos()
    ));
    fs::write(&temp_path, bytes)
        .with_context(|| format!("failed to write {}", temp_path.display()))?;
    Ok(temp_path)
}

fn ensure_guest_dir_ext4(rootfs: &Path, path: &str) -> anyhow::Result<()> {
    if path.is_empty() {
        return Ok(());
    }

    let mut current = String::new();
    for segment in path.split('/') {
        if segment.is_empty() {
            continue;
        }
        current.push('/');
        current.push_str(segment);
        debugfs_mkdir(rootfs, &current)?;
    }
    Ok(())
}

fn debugfs_write_file(rootfs: &Path, host_path: &Path, guest_path: &str) -> anyhow::Result<()> {
    debugfs_run(
        rootfs,
        &format!("write {} {}", host_path.display(), guest_path),
    )
}

fn debugfs_mkdir(rootfs: &Path, guest_dir: &str) -> anyhow::Result<()> {
    let output = Command::new("debugfs")
        .arg("-w")
        .arg("-R")
        .arg(format!("mkdir {guest_dir}"))
        .arg(rootfs)
        .output()
        .with_context(|| format!("failed to spawn debugfs for {}", rootfs.display()))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stderr.contains("already exists") || stdout.contains("already exists") {
        return Ok(());
    }

    anyhow::bail!(
        "debugfs mkdir failed for {}: `{}`\nstdout:\n{}\nstderr:\n{}",
        rootfs.display(),
        guest_dir,
        stdout.trim(),
        stderr.trim()
    )
}

fn debugfs_run(rootfs: &Path, command: &str) -> anyhow::Result<()> {
    let output = Command::new("debugfs")
        .arg("-w")
        .arg("-R")
        .arg(command)
        .arg(rootfs)
        .output()
        .with_context(|| format!("failed to spawn debugfs for {}", rootfs.display()))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        anyhow::bail!(
            "debugfs command failed for {}: `{}`\nstdout:\n{}\nstderr:\n{}",
            rootfs.display(),
            command,
            stdout.trim(),
            stderr.trim()
        )
    }
}

fn ensure_ext4_capacity(rootfs: &Path, incoming_bytes: u64) -> anyhow::Result<()> {
    let stats = ext4_stats(rootfs)?;
    let free_bytes = stats.free_blocks.saturating_mul(stats.block_size);
    let required_free = incoming_bytes.saturating_add(EXT4_MIN_FREE_BYTES_AFTER_INJECT);
    if free_bytes >= required_free {
        return Ok(());
    }

    let grow_bytes = EXT4_MIN_GROW_BYTES.max(required_free.saturating_sub(free_bytes));
    grow_ext4_image(rootfs, grow_bytes)
}

fn grow_ext4_image(rootfs: &Path, grow_bytes: u64) -> anyhow::Result<()> {
    let metadata = fs::metadata(rootfs)
        .with_context(|| format!("failed to stat rootfs {}", rootfs.display()))?;
    let new_len = metadata.len().saturating_add(grow_bytes);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(rootfs)
        .with_context(|| format!("failed to open rootfs {}", rootfs.display()))?;
    file.set_len(new_len)
        .with_context(|| format!("failed to resize rootfs {} to {new_len}", rootfs.display()))?;

    let output = Command::new("resize2fs")
        .arg(rootfs)
        .output()
        .with_context(|| format!("failed to spawn resize2fs for {}", rootfs.display()))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    anyhow::bail!(
        "resize2fs failed for {}\nstdout:\n{}\nstderr:\n{}",
        rootfs.display(),
        stdout.trim(),
        stderr.trim()
    )
}

fn ext4_stats(rootfs: &Path) -> anyhow::Result<Ext4Stats> {
    let output = Command::new("debugfs")
        .arg("-R")
        .arg("stats")
        .arg(rootfs)
        .output()
        .with_context(|| format!("failed to spawn debugfs stats for {}", rootfs.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        anyhow::bail!(
            "debugfs stats failed for {}\nstdout:\n{}\nstderr:\n{}",
            rootfs.display(),
            stdout.trim(),
            stderr.trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let block_size = parse_ext4_stat_value(&stdout, "Block size")?;
    let free_blocks = parse_ext4_stat_value(&stdout, "Free blocks")?;
    Ok(Ext4Stats {
        block_size,
        free_blocks,
    })
}

fn parse_ext4_stat_value(output: &str, key: &str) -> anyhow::Result<u64> {
    output
        .lines()
        .find_map(|line| {
            let (field, value) = line.split_once(':')?;
            (field.trim() == key)
                .then(|| value.split_whitespace().next())
                .flatten()
                .and_then(|raw| raw.parse::<u64>().ok())
        })
        .ok_or_else(|| anyhow::anyhow!("failed to parse `{key}` from debugfs stats output"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ext4Stats {
    block_size: u64,
    free_blocks: u64,
}

fn ensure_guest_dir<IO>(
    fs: &FileSystem<IO, fatfs::DefaultTimeProvider, LossyOemCpConverter>,
    path: &str,
) -> anyhow::Result<()>
where
    IO: fatfs::ReadWriteSeek,
{
    if path.is_empty() {
        return Ok(());
    }

    let mut current = String::new();
    for segment in path.split('/') {
        if segment.is_empty() {
            continue;
        }
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(segment);

        match fs.root_dir().open_dir(&current) {
            Ok(_) => {}
            Err(_) => {
                fs.root_dir().create_dir(&current).map_err(|err| {
                    anyhow::anyhow!("failed to create guest directory `{current}`: {err:?}")
                })?;
            }
        }
    }
    Ok(())
}

fn normalize_guest_path(path: &str) -> anyhow::Result<String> {
    let normalized = path.trim();
    if normalized.is_empty() {
        anyhow::bail!("guest path must not be empty");
    }
    let normalized = normalized.trim_start_matches('/');
    if normalized.is_empty() {
        anyhow::bail!("guest path must not be root");
    }
    Ok(normalized.to_string())
}

fn split_guest_path(path: &str) -> anyhow::Result<(&str, &str)> {
    match path.rsplit_once('/') {
        Some((parent, file_name)) if !file_name.is_empty() => Ok((parent, file_name)),
        Some((_parent, _file_name)) => anyhow::bail!("guest file name must not be empty"),
        None => Ok(("", path)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootfsFormat {
    Fat,
    Ext4,
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Read};

    use fatfs::{FormatVolumeOptions, StdIoWrapper, format_volume};
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn copy_file_creates_parent_and_copies_bytes() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("rootfs.img");
        let dst = dir.path().join("nested/run/rootfs.img");
        fs::write(&src, b"rootfs-bytes").unwrap();

        copy_file(&src, &dst).unwrap();

        assert_eq!(fs::read(&dst).unwrap(), b"rootfs-bytes");
    }

    #[test]
    fn generate_run_id_has_expected_prefix() {
        let run_id = generate_run_id().unwrap();
        assert!(run_id.starts_with("run-"));
        assert!(run_id.contains('-'));
    }

    #[test]
    fn inject_text_file_writes_content_into_fat_rootfs() {
        let dir = tempdir().unwrap();
        let rootfs = dir.path().join("rootfs.img");
        create_test_fat_image(&rootfs).unwrap();

        inject_text_file(&rootfs, "/axcases/meta/hello.txt", "hello fatfs").unwrap();

        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&rootfs)
            .unwrap();
        let fs = FileSystem::new(file, FsOptions::new()).unwrap();
        let mut content = String::new();
        fs.root_dir()
            .open_file("axcases/meta/hello.txt")
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "hello fatfs");
    }

    #[test]
    fn inject_host_file_copies_bytes_into_fat_rootfs() {
        let dir = tempdir().unwrap();
        let rootfs = dir.path().join("rootfs.img");
        let host = dir.path().join("payload.bin");
        create_test_fat_image(&rootfs).unwrap();
        fs::write(&host, b"\x01\x02\x03").unwrap();

        inject_host_file(&rootfs, "/axcases/data/payload.bin", &host).unwrap();

        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&rootfs)
            .unwrap();
        let fs = FileSystem::new(file, FsOptions::new()).unwrap();
        let mut bytes = Vec::new();
        fs.root_dir()
            .open_file("axcases/data/payload.bin")
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"\x01\x02\x03");
    }

    fn create_test_fat_image(path: &Path) -> anyhow::Result<()> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        file.set_len(1024 * 1024)
            .with_context(|| format!("failed to resize {}", path.display()))?;
        let mut wrapper = StdIoWrapper::new(file);
        format_volume(&mut wrapper, FormatVolumeOptions::new())
            .map_err(|err| anyhow::anyhow!("failed to format FAT image: {err:?}"))?;
        Ok(())
    }

    #[test]
    fn parse_ext4_stat_value_extracts_numeric_fields() {
        let stats = "\
Free blocks:              1234
Block size:               4096
";
        assert_eq!(parse_ext4_stat_value(stats, "Free blocks").unwrap(), 1234);
        assert_eq!(parse_ext4_stat_value(stats, "Block size").unwrap(), 4096);
    }
}
