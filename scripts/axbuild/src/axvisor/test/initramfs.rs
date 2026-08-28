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
// These bounds are the fixed Q35 guest aperture used by the x86 AxVM provider;
// the end bound is exclusive.
// Keep them synchronized with `virtualization/axvm/src/arch/x86_64/pci_config.rs`.
const X86_PCI_MEMORY_APERTURE_START: &str = "0xc0000000";
const X86_PCI_MEMORY_APERTURE_END: &str = "0xd0000000";
const INIT_SCRIPT_TEMPLATE: &str = r#"#!/bin/busybox sh
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

__AXVISOR_PCI_BAR_VALIDATOR__
__AXVISOR_PCI_CAPABILITY_VALIDATOR__

run_pci_enumeration_check() {
  success_marker=$1
  failed=0
  # The managed rootfs images ship no pciutils, so this check consumes the
  # kernel-published sysfs PCI state directly (the same source `lspci` reads).
  bdf=""
  count=0
  for dev in /sys/bus/pci/devices/*; do
    [ -d "$dev" ] || continue
    vendor=$(/bin/busybox cat "$dev/vendor")
    device=$(/bin/busybox cat "$dev/device")
    class=$(/bin/busybox cat "$dev/class")
    if [ "$vendor" = "0x1af4" ] && [ "$device" = "0x1110" ] && [ "$class" = "0x050000" ]; then
      bdf=${dev##*/}
      count=$((count + 1))
    fi
  done
  echo "guest kernel: $(/bin/busybox uname -r)"
  if [ "$count" -ne 1 ]; then
    echo "expected exactly one 0500:1af4:1110 function, found $count"
    failed=1
  else
    endpoint=/sys/bus/pci/devices/$bdf
    if [ -L "$endpoint/driver" ]; then
      echo "vPCI endpoint unexpectedly bound to a driver"
      failed=1
    fi
    # The kernel publishes `resource` entries as bare hex columns; tolerate an
    # optional 0x prefix and turn malformed input into an explicit failure so
    # the case reports FAILED instead of dying inside arithmetic.
    resource_line=$(/bin/busybox sed -n '3p' "$endpoint/resource")
    if ! validate_pci_bar_resource "$resource_line" __AXVISOR_PCI_MEMORY_APERTURE_START__ __AXVISOR_PCI_MEMORY_APERTURE_END__; then
      failed=1
      echo "BAR2 is not a valid 64 KiB memory resource"
    fi
    if ! validate_pci_capabilities "$endpoint/config"; then
      failed=1
    fi
    fi
  if [ "$failed" -ne 0 ]; then
    echo AXVISOR_X86_VPCI_ENUMERATION_FAILED
  else
    echo "$success_marker"
  fi
}

cmdline=$(/bin/busybox cat /proc/cmdline)
case "$cmdline" in
  *axvisor.acpi_case=direct*) run_x86_acpi_check AXVISOR_X86_DIRECT_ACPI_PASSED; exec /bin/busybox sh -i ;;
  *axvisor.acpi_case=ovmf*) run_x86_acpi_check AXVISOR_X86_OVMF_ACPI_PASSED; exec /bin/busybox sh -i ;;
  *axvisor.pci_case=enumeration*) run_pci_enumeration_check AXVISOR_X86_VPCI_ENUMERATION_PASSED; exec /bin/busybox sh -i ;;
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

const PCI_BAR_VALIDATOR: &str = r#"validate_pci_bar_resource() {
  resource_line=$1
  aperture_start=$2
  aperture_end=$3
  aperture_start=${aperture_start#0x}
  aperture_end=${aperture_end#0x}
  set -- $resource_line
  if [ $# -lt 3 ]; then
    echo "BAR2 resource entry is missing or unreadable"
    return 1
  fi
  v1=${1#0x}
  v2=${2#0x}
  v3=${3#0x}
  case "$v1$v2$v3$aperture_start$aperture_end" in
    *[!0-9a-fA-F]*)
      echo "BAR2 resource entry is malformed: $resource_line"
      return 1
      ;;
  esac
  bar_start=$((0x$v1))
  bar_end=$((0x$v2))
  bar_flags=$((0x$v3))
  echo "vPCI endpoint BAR2 [$bar_start-$bar_end] flags $bar_flags"
  if [ "$bar_end" -lt "$bar_start" ]; then
    echo "BAR2 resource range is inverted"
    return 1
  fi
  if [ "$bar_start" -eq 0 ]; then
    echo "BAR2 has an unassigned base address"
    return 1
  fi
  aperture_start=$((0x$aperture_start))
  aperture_end=$((0x$aperture_end))
  if [ "$bar_start" -lt "$aperture_start" ] || [ "$bar_end" -ge "$aperture_end" ]; then
    echo "BAR2 is outside the PCI memory aperture"
    return 1
  fi
  bar_size=$((bar_end - bar_start + 1))
  if [ "$bar_size" -ne 65536 ]; then
    echo "BAR2 is not a 64 KiB memory resource"
    return 1
  fi
  if [ $((bar_flags & 0x200)) -eq 0 ]; then
    echo "BAR2 is not a memory resource"
    return 1
  fi
  if [ $((bar_flags & 0x2000)) -ne 0 ]; then
    echo "BAR2 unexpectedly prefetchable"
    return 1
  fi
  if [ $((bar_flags & 0x100000)) -ne 0 ]; then
    echo "BAR2 unexpectedly uses a 64-bit memory resource"
    return 1
  fi
  if [ $((bar_flags & 0x20000000)) -ne 0 ]; then
    echo "BAR2 has an unassigned resource flag"
    return 1
  fi
  return 0
}
"#;

const PCI_CAPABILITY_VALIDATOR: &str = r#"validate_pci_capabilities() {
  config_path=$1
  if [ ! -r "$config_path" ]; then
    echo "PCI configuration space is missing or unreadable"
    return 1
  fi
  read_config_byte() {
    bytes=$(/bin/od -An -tx1 -j "$2" -N 1 "$1" 2>/dev/null) || return 1
    set -- $bytes
    if [ $# -ne 1 ]; then
      return 1
    fi
    value=${1#0x}
    case "$value" in
      ''|*[!0-9a-fA-F]*) return 1 ;;
    esac
    printf '%s\n' "$value"
  }

  status_low=$(read_config_byte "$config_path" 6) || {
    echo "PCI status register is unreadable"
    return 1
  }
  if [ $((0x$status_low & 0x10)) -eq 0 ]; then
    return 0
  fi

  pointer_hex=$(read_config_byte "$config_path" 52) || {
    echo "PCI capability pointer is unreadable"
    return 1
  }
  pointer=$((0x$pointer_hex))
  if [ "$pointer" -eq 0 ]; then
    echo "PCI capability list is missing its first entry"
    return 1
  fi
  visited=:
  iteration=0
  while [ "$pointer" -ne 0 ] && [ "$iteration" -lt 48 ]; do
    case ":$visited:" in
      *":$pointer:"*)
        echo "PCI capability list contains a cycle"
        return 1
        ;;
    esac
    visited="$visited:$pointer"
    if [ "$pointer" -lt 64 ] || [ "$pointer" -gt 252 ] || [ $((pointer % 4)) -ne 0 ]; then
      echo "PCI capability pointer is invalid: $pointer"
      return 1
    fi
    capability_hex=$(read_config_byte "$config_path" "$pointer") || {
      echo "PCI capability ID is unreadable"
      return 1
    }
    capability=$((0x$capability_hex))
    if [ "$capability" -eq 5 ] || [ "$capability" -eq 17 ]; then
      echo "vPCI endpoint unexpectedly advertises MSI/MSI-X"
      return 1
    fi
    if [ "$capability" -eq 0 ]; then
      echo "PCI capability ID is invalid"
      return 1
    fi
    next_offset=$((pointer + 1))
    next_hex=$(read_config_byte "$config_path" "$next_offset") || {
      echo "PCI capability next pointer is unreadable"
      return 1
    }
    pointer=$((0x$next_hex))
    if [ "$pointer" -ne 0 ] &&
       { [ "$pointer" -lt 64 ] || [ "$pointer" -gt 252 ] || [ $((pointer % 4)) -ne 0 ]; }; then
      echo "PCI capability next pointer is invalid: $pointer"
      return 1
    fi
    iteration=$((iteration + 1))
  done
  if [ "$pointer" -ne 0 ]; then
    echo "PCI capability list is too long"
    return 1
  fi
  return 0
}
"#;

fn init_script() -> Vec<u8> {
    INIT_SCRIPT_TEMPLATE
        .replace("__AXVISOR_PCI_BAR_VALIDATOR__", PCI_BAR_VALIDATOR)
        .replace(
            "__AXVISOR_PCI_CAPABILITY_VALIDATOR__",
            PCI_CAPABILITY_VALIDATOR,
        )
        .replace(
            "__AXVISOR_PCI_MEMORY_APERTURE_START__",
            X86_PCI_MEMORY_APERTURE_START,
        )
        .replace(
            "__AXVISOR_PCI_MEMORY_APERTURE_END__",
            X86_PCI_MEMORY_APERTURE_END,
        )
        .into_bytes()
}

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
        let evidence = super::ovmf::prepare_x86_ovmf(&output_path).await?;
        println!("{evidence}");
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
    let init_script = init_script();
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
        archive.append_regular("init", &init_script)?;
        for applet in [
            "cat", "date", "dmesg", "grep", "mount", "od", "sed", "sh", "sleep",
        ] {
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
    #[cfg(unix)]
    use std::fs;
    use std::io::Read;
    #[cfg(unix)]
    use std::process::Command;

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
            init.windows(b"validate_pci_bar_resource".len())
                .any(|window| window == b"validate_pci_bar_resource")
        );
        assert!(
            init.windows(b"validate_pci_capabilities".len())
                .any(|window| window == b"validate_pci_capabilities")
        );
        assert!(
            !init
                .windows(b"__AXVISOR_PCI_MEMORY_APERTURE_START__".len())
                .any(|window| window == b"__AXVISOR_PCI_MEMORY_APERTURE_START__")
        );
        assert!(
            !init
                .windows(b"__AXVISOR_PCI_CAPABILITY_VALIDATOR__".len())
                .any(|window| window == b"__AXVISOR_PCI_CAPABILITY_VALIDATOR__")
        );
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
        assert!(
            init.windows(b"AXVISOR_X86_VPCI_ENUMERATION_PASSED".len())
                .any(|window| window == b"AXVISOR_X86_VPCI_ENUMERATION_PASSED")
        );
        assert!(
            init.windows(b"AXVISOR_X86_VPCI_ENUMERATION_FAILED".len())
                .any(|window| window == b"AXVISOR_X86_VPCI_ENUMERATION_FAILED")
        );
        for applet in [
            "cat", "date", "dmesg", "grep", "mount", "od", "sed", "sh", "sleep",
        ] {
            assert_eq!(entries.get(&format!("bin/{applet}")).unwrap(), b"busybox");
        }
    }

    #[cfg(unix)]
    #[test]
    fn pci_bar_validator_rejects_unassigned_zero_based_resource() {
        let output = run_pci_bar_validator(
            "0000000000000000 000000000000ffff 00000200",
            X86_PCI_MEMORY_APERTURE_START,
            X86_PCI_MEMORY_APERTURE_END,
        );
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_bar_validator_rejects_resource_outside_aperture() {
        let output = run_pci_bar_validator(
            "00000000b0000000 00000000b000ffff 00000200",
            X86_PCI_MEMORY_APERTURE_START,
            X86_PCI_MEMORY_APERTURE_END,
        );
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_bar_validator_rejects_unassigned_resource_flag() {
        let output = run_pci_bar_validator(
            "00000000c0000000 00000000c000ffff 20000200",
            X86_PCI_MEMORY_APERTURE_START,
            X86_PCI_MEMORY_APERTURE_END,
        );
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_bar_validator_rejects_64_bit_memory_resource() {
        let output = run_pci_bar_validator(
            "00000000c0000000 00000000c000ffff 00100200",
            X86_PCI_MEMORY_APERTURE_START,
            X86_PCI_MEMORY_APERTURE_END,
        );
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_bar_validator_accepts_assigned_resource_inside_aperture() {
        let output = run_pci_bar_validator(
            "00000000c0000000 00000000c000ffff 00000200",
            X86_PCI_MEMORY_APERTURE_START,
            X86_PCI_MEMORY_APERTURE_END,
        );
        assert!(output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_capability_validator_rejects_msi_capability() {
        let mut config = vec![0; 256];
        config[0x06] = 0x10;
        config[0x34] = 0x40;
        config[0x40] = 0x05;
        let output = run_pci_capability_validator(&config);
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_capability_validator_rejects_msix_capability() {
        let mut config = vec![0; 256];
        config[0x06] = 0x10;
        config[0x34] = 0x40;
        config[0x40] = 0x11;
        let output = run_pci_capability_validator(&config);
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_capability_validator_rejects_msi_after_another_capability() {
        let mut config = vec![0; 256];
        config[0x06] = 0x10;
        config[0x34] = 0x40;
        config[0x40] = 0x01;
        config[0x41] = 0x44;
        config[0x44] = 0x05;
        let output = run_pci_capability_validator(&config);
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_capability_validator_rejects_invalid_capability_pointer() {
        let mut config = vec![0; 256];
        config[0x06] = 0x10;
        config[0x34] = 0x42;
        let output = run_pci_capability_validator(&config);
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_capability_validator_rejects_capability_cycle() {
        let mut config = vec![0; 256];
        config[0x06] = 0x10;
        config[0x34] = 0x40;
        config[0x40] = 0x01;
        config[0x41] = 0x40;
        let output = run_pci_capability_validator(&config);
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_capability_validator_accepts_config_without_capability_list() {
        let output = run_pci_capability_validator(&[0; 256]);
        assert!(output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_capability_validator_accepts_a_non_msi_capability_list() {
        let mut config = vec![0; 256];
        config[0x06] = 0x10;
        config[0x34] = 0x40;
        config[0x40] = 0x01;
        let output = run_pci_capability_validator(&config);
        assert!(output.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn pci_capability_validator_rejects_missing_first_capability() {
        let mut config = vec![0; 256];
        config[0x06] = 0x10;
        let output = run_pci_capability_validator(&config);
        assert!(!output.status.success());
    }

    #[cfg(unix)]
    fn run_pci_bar_validator(
        resource_line: &str,
        aperture_start: &str,
        aperture_end: &str,
    ) -> std::process::Output {
        Command::new("sh")
            .arg("-c")
            .arg(format!(
                "{PCI_BAR_VALIDATOR}\nvalidate_pci_bar_resource \"$1\" \"$2\" \"$3\""
            ))
            .arg("pci-bar-test")
            .arg(resource_line)
            .arg(aperture_start)
            .arg(aperture_end)
            .output()
            .unwrap()
    }

    #[cfg(unix)]
    fn run_pci_capability_validator(config: &[u8]) -> std::process::Output {
        let directory = tempdir().unwrap();
        let config_path = directory.path().join("config");
        fs::write(&config_path, config).unwrap();
        Command::new("sh")
            .arg("-c")
            .arg(format!(
                "{PCI_CAPABILITY_VALIDATOR}\nvalidate_pci_capabilities \"$1\""
            ))
            .arg("pci-capability-test")
            .arg(config_path)
            .output()
            .unwrap()
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
