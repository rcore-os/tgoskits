//! Minimal initramfs generation for Axvisor QEMU tests.
//!
//! Timer and interrupt-controller tests must not depend on a guest block
//! device reaching its interrupt handler. A build group can therefore request
//! a small BusyBox initramfs generated from the managed architecture rootfs.

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, bail, ensure};
use flate2::{Compression, write::GzEncoder};
use ostool::build::config::Cargo;
use tempfile::NamedTempFile;

use crate::{axvisor::rootfs, context::ResolvedAxvisorRequest, rootfs::inject::read_binary_file};

const OUTPUT_ENV: &str = "AXVISOR_TEST_BUSYBOX_INITRAMFS";
const OVMF_OUTPUT_ENV: &str = "AXVISOR_TEST_X86_OVMF_OUTPUT";
const BUSYBOX_PATH: &str = "/bin/busybox";
const INIT_SCRIPT: &[u8] = br#"#!/bin/busybox sh
/bin/busybox mount -t devtmpfs devtmpfs /dev 2>/dev/null || true
/bin/busybox mount -t proc proc /proc 2>/dev/null || true
/bin/busybox mount -t sysfs sysfs /sys 2>/dev/null || true
export HOME=/root
export PATH=/bin
export TERM=vt100
export PS1='~ # '
cd /root

run_x86_acpi_check() {
  success_marker=$1
  failed=0
  # Linux uses the XSDT to discover these tables but does not export the root
  # XSDT itself through /sys/firmware/acpi/tables.
  for table in DSDT APIC FACP SPCR; do
    if [ ! -r "/sys/firmware/acpi/tables/$table" ]; then
      echo "missing readable ACPI table: $table"
      failed=1
    fi
  done
  online=$(/bin/busybox cat /sys/devices/system/cpu/online 2>/dev/null)
  if [ "$online" != "0" ]; then
    echo "unexpected online CPU set: $online"
    failed=1
  fi
  if [ ! -e /sys/class/tty/ttyS0 ]; then
    echo "missing ttyS0"
    failed=1
  fi
  if ! /bin/busybox grep -q 'IO-APIC' /proc/interrupts; then
    echo "Linux did not initialize an IOAPIC"
    failed=1
  fi
  if [ "$failed" -ne 0 ]; then
    echo AXVISOR_X86_ACPI_FAILED
  else
    echo "$success_marker"
  fi
}

cmdline=$(/bin/busybox cat /proc/cmdline)
case "$cmdline" in
  *axvisor.acpi_case=direct*) run_x86_acpi_check AXVISOR_X86_DIRECT_ACPI_PASSED; exec /bin/busybox sh -i ;;
  *axvisor.acpi_case=ovmf*) run_x86_acpi_check AXVISOR_X86_OVMF_ACPI_PASSED; exec /bin/busybox sh -i ;;
  *axvisor.acpi_case=off*)
    if [ -d /sys/firmware/acpi/tables ]; then
      echo AXVISOR_X86_ACPI_FAILED
    else
      echo AXVISOR_X86_MP_FALLBACK_PASSED
    fi
    exec /bin/busybox sh -i
    ;;
  *axvisor.timer_case=gicv3-its*) success_marker=AXVISOR_GICV3_ITS_TIMER_STRESS_PASSED; require_its=1 ;;
  *axvisor.timer_case=gicv2*) success_marker=AXVISOR_GICV2_TIMER_STRESS_PASSED; require_its=0 ;;
  *axvisor.timer_case=gicv3*) success_marker=AXVISOR_GICV3_TIMER_STRESS_PASSED; require_its=0 ;;
  *) echo AXVISOR_GUEST_ASSERTION_CASE_UNKNOWN; exec /bin/busybox sh -i ;;
esac

start=$(/bin/busybox date +%s)
last=$start
round=0
failed=0
while [ "$round" -lt 8 ]; do
  /bin/busybox sleep 1 & p1=$!
  /bin/busybox sleep 2 & p2=$!
  /bin/busybox sleep 3 & p3=$!
  wait "$p1"
  wait "$p2"
  wait "$p3"
  now=$(/bin/busybox date +%s)
  if [ "$now" -lt "$last" ]; then
    failed=1
  fi
  last=$now
  round=$((round + 1))
done
end=$(/bin/busybox date +%s)
elapsed=$((end - start))
if [ "$require_its" -eq 1 ] && ! /bin/busybox dmesg | /bin/busybox grep -q 'ITS'; then
  failed=1
fi
if [ "$failed" -ne 0 ] || [ "$elapsed" -lt 20 ] || [ "$elapsed" -gt 120 ]; then
  echo TIMER_STRESS_FAILED
else
  echo "$success_marker"
fi

exec /bin/busybox sh -i
"#;

pub(super) async fn prepare_configured_busybox_initramfs(
    request: &ResolvedAxvisorRequest,
    cargo: &Cargo,
    workspace_root: &Path,
) -> anyhow::Result<()> {
    if let Some(configured_output) = cargo.env.get(OUTPUT_ENV) {
        let output_path = resolve_output_path(workspace_root, configured_output, OUTPUT_ENV)?;
        let rootfs_path = rootfs::qemu_rootfs_path(request, workspace_root, None)?;
        prepare_busybox_initramfs(&rootfs_path, &output_path, &request.arch)?;
        println!(
            "prepared Axvisor QEMU test initramfs: {}",
            output_path.display()
        );
    }
    if let Some(configured_output) = cargo.env.get(OVMF_OUTPUT_ENV) {
        ensure!(
            request.arch == "x86_64",
            "{OVMF_OUTPUT_ENV} is only valid for x86_64 Axvisor tests"
        );
        let output_path = resolve_output_path(workspace_root, configured_output, OVMF_OUTPUT_ENV)?;
        super::ovmf::prepare_x86_ovmf(&output_path).await?;
        println!("prepared Axvisor x86 OVMF image: {}", output_path.display());
    }
    Ok(())
}

fn resolve_output_path(
    workspace_root: &Path,
    configured_output: &str,
    variable: &str,
) -> anyhow::Result<PathBuf> {
    let configured_output = Path::new(configured_output);
    ensure!(
        !configured_output.is_absolute()
            && configured_output
                .components()
                .all(|component| matches!(component, Component::CurDir | Component::Normal(_))),
        "{variable} must be a workspace-relative path without parent traversal"
    );
    Ok(workspace_root.join(configured_output))
}

fn prepare_busybox_initramfs(
    rootfs_path: &Path,
    output_path: &Path,
    arch: &str,
) -> anyhow::Result<()> {
    let busybox = required_rootfs_file(rootfs_path, BUSYBOX_PATH)?;
    let loader_path = musl_loader_path(arch)?;
    let loader = required_rootfs_file(rootfs_path, loader_path)?;
    let archive = build_busybox_initramfs(&busybox, loader_path, &loader)?;

    let output_parent = output_path.parent().with_context(|| {
        format!(
            "initramfs output path has no parent: {}",
            output_path.display()
        )
    })?;
    fs::create_dir_all(output_parent).with_context(|| {
        format!(
            "failed to create initramfs output directory {}",
            output_parent.display()
        )
    })?;
    let mut temporary = NamedTempFile::new_in(output_parent).with_context(|| {
        format!(
            "failed to create temporary initramfs in {}",
            output_parent.display()
        )
    })?;
    temporary
        .write_all(&archive)
        .with_context(|| format!("failed to write {}", output_path.display()))?;
    temporary
        .persist(output_path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to install {}", output_path.display()))?;
    Ok(())
}

fn required_rootfs_file(rootfs_path: &Path, guest_path: &str) -> anyhow::Result<Vec<u8>> {
    read_binary_file(rootfs_path, guest_path)?.with_context(|| {
        format!(
            "managed rootfs {} does not contain required file {guest_path}",
            rootfs_path.display()
        )
    })
}

fn musl_loader_path(arch: &str) -> anyhow::Result<&'static str> {
    match arch {
        "aarch64" => Ok("/lib/ld-musl-aarch64.so.1"),
        "loongarch64" => Ok("/lib/ld-musl-loongarch64.so.1"),
        "riscv64" => Ok("/lib/ld-musl-riscv64.so.1"),
        "x86_64" => Ok("/lib/ld-musl-x86_64.so.1"),
        unsupported => {
            bail!("BusyBox test initramfs does not support architecture `{unsupported}`")
        }
    }
}

fn build_busybox_initramfs(
    busybox: &[u8],
    loader_path: &str,
    loader: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    {
        let mut archive = NewcArchive::new(&mut encoder);
        let mut directories = BTreeSet::from([
            "bin".to_string(),
            "dev".to_string(),
            "proc".to_string(),
            "root".to_string(),
            "sys".to_string(),
            "tmp".to_string(),
        ]);
        let loader_archive_path = archive_path(loader_path)?;
        add_parent_directories(loader_archive_path, &mut directories);
        for directory in directories {
            archive.append_directory(&directory)?;
        }

        archive.append_regular("bin/busybox", busybox)?;
        archive.append_regular(loader_archive_path, loader)?;
        archive.append_regular("init", INIT_SCRIPT)?;
        for applet in ["cat", "date", "dmesg", "grep", "mount", "sh", "sleep"] {
            archive.append_symlink(&format!("bin/{applet}"), "busybox")?;
        }
        archive.finish()?;
    }
    encoder
        .finish()
        .context("failed to finish initramfs gzip stream")
}

fn archive_path(guest_path: &str) -> anyhow::Result<&str> {
    let archive_path = guest_path
        .strip_prefix('/')
        .context("initramfs guest path must be absolute")?;
    ensure!(
        !archive_path.is_empty()
            && Path::new(archive_path)
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
        "invalid initramfs guest path `{guest_path}`"
    );
    Ok(archive_path)
}

fn add_parent_directories(path: &str, directories: &mut BTreeSet<String>) {
    let mut parent = Path::new(path).parent();
    while let Some(path) = parent {
        if path.as_os_str().is_empty() {
            break;
        }
        directories.insert(path.to_string_lossy().into_owned());
        parent = path.parent();
    }
}

struct NewcArchive<W> {
    writer: W,
    inode: u32,
}

impl<W: Write> NewcArchive<W> {
    fn new(writer: W) -> Self {
        Self { writer, inode: 1 }
    }

    fn append_directory(&mut self, path: &str) -> anyhow::Result<()> {
        self.append(path, 0o040755, 2, &[])
    }

    fn append_regular(&mut self, path: &str, contents: &[u8]) -> anyhow::Result<()> {
        self.append(path, 0o100755, 1, contents)
    }

    fn append_symlink(&mut self, path: &str, target: &str) -> anyhow::Result<()> {
        self.append(path, 0o120777, 1, target.as_bytes())
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        self.append("TRAILER!!!", 0, 1, &[])
    }

    fn append(
        &mut self,
        path: &str,
        mode: u32,
        link_count: u32,
        contents: &[u8],
    ) -> anyhow::Result<()> {
        ensure!(!path.as_bytes().contains(&0), "cpio path contains NUL");
        let file_size = u32::try_from(contents.len()).context("cpio entry is larger than 4 GiB")?;
        let name_size = u32::try_from(path.len() + 1).context("cpio path is too long")?;
        write!(
            self.writer,
            "070701{:08x}{mode:08x}{:08x}{:08x}{link_count:08x}{:08x}{file_size:08x}{:08x}{:08x}{:\
             08x}{:08x}{name_size:08x}{:08x}",
            self.inode, 0, 0, 0, 0, 0, 0, 0, 0,
        )
        .context("failed to write cpio header")?;
        self.writer
            .write_all(path.as_bytes())
            .context("failed to write cpio path")?;
        self.writer
            .write_all(&[0])
            .context("failed to terminate cpio path")?;
        write_padding(&mut self.writer, 110 + path.len() + 1)?;
        self.writer
            .write_all(contents)
            .context("failed to write cpio contents")?;
        write_padding(&mut self.writer, contents.len())?;
        self.inode = self.inode.checked_add(1).context("cpio inode overflow")?;
        Ok(())
    }
}

fn write_padding(writer: &mut impl Write, written: usize) -> anyhow::Result<()> {
    const ZEROES: [u8; 3] = [0; 3];
    let padding = (4 - written % 4) % 4;
    writer
        .write_all(&ZEROES[..padding])
        .context("failed to write cpio alignment")
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use flate2::read::GzDecoder;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn configured_output_must_stay_inside_workspace() {
        let root = tempdir().unwrap();

        assert_eq!(
            resolve_output_path(root.path(), "tmp/initramfs.cpio.gz", OUTPUT_ENV).unwrap(),
            root.path().join("tmp/initramfs.cpio.gz")
        );
        assert!(resolve_output_path(root.path(), "../outside", OUTPUT_ENV).is_err());
        assert!(resolve_output_path(root.path(), "/tmp/outside", OUTPUT_ENV).is_err());
    }

    #[test]
    fn generated_archive_contains_busybox_loader_and_shell_applets() {
        let compressed =
            build_busybox_initramfs(b"busybox", "/lib/ld-musl-test.so.1", b"loader").unwrap();
        let mut archive = Vec::new();
        GzDecoder::new(compressed.as_slice())
            .read_to_end(&mut archive)
            .unwrap();
        let entries = parse_newc_entries(&archive);

        assert_eq!(entries.get("bin/busybox").unwrap(), b"busybox");
        assert_eq!(entries.get("lib/ld-musl-test.so.1").unwrap(), b"loader");
        let init = entries.get("init").unwrap();
        assert!(init.starts_with(b"#!/bin/busybox sh"));
        assert!(
            init.windows(b"AXVISOR_GICV2_TIMER_STRESS_PASSED".len())
                .any(|window| window == b"AXVISOR_GICV2_TIMER_STRESS_PASSED")
        );
        assert!(
            init.windows(b"AXVISOR_GICV3_TIMER_STRESS_PASSED".len())
                .any(|window| window == b"AXVISOR_GICV3_TIMER_STRESS_PASSED")
        );
        assert!(
            init.windows(b"AXVISOR_GICV3_ITS_TIMER_STRESS_PASSED".len())
                .any(|window| window == b"AXVISOR_GICV3_ITS_TIMER_STRESS_PASSED")
        );
        assert!(
            init.windows(b"AXVISOR_X86_DIRECT_ACPI_PASSED".len())
                .any(|window| window == b"AXVISOR_X86_DIRECT_ACPI_PASSED")
        );
        assert!(
            init.windows(b"AXVISOR_X86_OVMF_ACPI_PASSED".len())
                .any(|window| window == b"AXVISOR_X86_OVMF_ACPI_PASSED")
        );
        for applet in ["cat", "date", "dmesg", "grep", "mount", "sh", "sleep"] {
            assert_eq!(entries.get(&format!("bin/{applet}")).unwrap(), b"busybox");
        }
    }

    fn parse_newc_entries(archive: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
        let mut entries = std::collections::BTreeMap::new();
        let mut offset = 0;
        loop {
            assert_eq!(&archive[offset..offset + 6], b"070701");
            let file_size = parse_hex(&archive[offset + 54..offset + 62]);
            let name_size = parse_hex(&archive[offset + 94..offset + 102]);
            offset += 110;
            let name = std::str::from_utf8(&archive[offset..offset + name_size - 1]).unwrap();
            offset = align4(offset + name_size);
            if name == "TRAILER!!!" {
                break;
            }
            entries.insert(
                name.to_string(),
                archive[offset..offset + file_size].to_vec(),
            );
            offset = align4(offset + file_size);
        }
        entries
    }

    fn parse_hex(bytes: &[u8]) -> usize {
        usize::from_str_radix(std::str::from_utf8(bytes).unwrap(), 16).unwrap()
    }

    fn align4(value: usize) -> usize {
        (value + 3) & !3
    }
}
