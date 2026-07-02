use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use object::{Object, ObjectSection};

#[derive(Clone, Copy)]
pub(super) struct AddressRange {
    pub(super) start: u64,
    pub(super) end: u64,
}

#[derive(Clone, Copy)]
pub(super) struct KernelTextRange {
    pub(super) virt: AddressRange,
    pub(super) phys: Option<AddressRange>,
}

pub(super) fn kernel_bin_path(root: &Path, target: &str, debug: bool) -> PathBuf {
    root.join("target")
        .join(target)
        .join(if debug { "debug" } else { "release" })
        .join("starryos.bin")
}

pub(super) fn detect_kernel_text_range(elf: &Path) -> anyhow::Result<Option<KernelTextRange>> {
    if !elf.exists() {
        eprintln!(
            "qperf: kernel ELF not found at {}, skipping .text range filter",
            elf.display()
        );
        return Ok(None);
    }
    let data =
        fs::read(elf).with_context(|| format!("failed to read kernel ELF {}", elf.display()))?;
    let obj = object::File::parse(&*data)
        .map_err(|err| anyhow::anyhow!("failed to parse kernel ELF: {err}"))?;
    let mut virt: Option<AddressRange> = None;
    for section in obj.sections() {
        if !matches!(section.name().unwrap_or(""), ".head.text" | ".text") {
            continue;
        }
        let start = section.address();
        let size = section.size();
        if start == 0 || size == 0 {
            continue;
        }
        let end = start
            .checked_add(size)
            .context("kernel text section end address overflow")?;
        virt = Some(match virt {
            Some(range) => AddressRange {
                start: range.start.min(start),
                end: range.end.max(end),
            },
            None => AddressRange { start, end },
        });
    }

    let Some(virt) = virt else {
        eprintln!(
            "qperf: could not find .head.text/.text sections in kernel ELF, address filter \
             disabled"
        );
        return Ok(None);
    };
    let size = virt.end - virt.start;
    let phys = detect_low_address_text_alias(virt);
    eprintln!(
        "qperf: detected kernel text virtual range: 0x{:x}..0x{:x} ({size} bytes)",
        virt.start, virt.end
    );
    if let Some(phys) = phys {
        eprintln!(
            "qperf: detected kernel text physical alias: 0x{:x}..0x{:x}",
            phys.start, phys.end
        );
    }
    Ok(Some(KernelTextRange { virt, phys }))
}

fn detect_low_address_text_alias(virt: AddressRange) -> Option<AddressRange> {
    const LOW_32BIT_MASK: u64 = 0x0000_0000_ffff_ffff;

    if virt.end <= LOW_32BIT_MASK || virt.start & !LOW_32BIT_MASK == 0 {
        return None;
    }
    let size = virt.end.checked_sub(virt.start)?;
    let start = virt.start & LOW_32BIT_MASK;
    let end = start.checked_add(size)?;
    Some(AddressRange { start, end })
}
