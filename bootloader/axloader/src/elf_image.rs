use std::{fs, path::Path};

use anyhow::{Context, anyhow, bail};
use object::{
    Object, ObjectSymbol,
    read::elf::{FileHeader, ProgramHeader},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentInfo {
    pub index: usize,
    pub vaddr: u64,
    pub paddr: u64,
    pub file_size: u64,
    pub mem_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElfImageReport {
    pub entry: u64,
    pub load_addr: u64,
    pub image_file_size: u64,
    pub image_mem_size: u64,
    pub entry_offset: u64,
    pub entry_paddr: u64,
    pub start_symbol: Option<u64>,
    pub httpboot_entry_symbol: Option<u64>,
    pub main_symbol: Option<u64>,
    pub load_segments: Vec<SegmentInfo>,
}

impl ElfImageReport {
    pub fn print_httpboot_report(&self, elf_path: &Path) {
        println!("=== Axvisor HTTP Boot ===");
        println!("elf: {}", elf_path.display());
        println!("kernel_load_addr: {}", hex(self.load_addr));
        println!("elf_entry: {}", hex(self.entry));
        println!("entry_offset_from_load: {}", hex(self.entry_offset));
        println!("entry_paddr_for_manifest: {}", hex(self.entry_paddr));
        println!("flat_binary_file_size: {}", hex(self.image_file_size));
        println!("flat_binary_memory_span: {}", hex(self.image_mem_size));

        for segment in &self.load_segments {
            println!(
                "load_segment[{}]: paddr={} vaddr={} filesz={} memsz={}",
                segment.index,
                hex(segment.paddr),
                hex(segment.vaddr),
                hex(segment.file_size),
                hex(segment.mem_size)
            );
        }

        println!("_start: {}", option_hex(self.start_symbol));
        println!("httpboot_entry: {}", option_hex(self.httpboot_entry_symbol));
        println!("main: {}", option_hex(self.main_symbol));
        if self.httpboot_entry_symbol.is_some() {
            println!("phase0_status: ready");
            println!(
                "phase0_note: image has a compact physical load span and exposes a direct HTTP \
                 Boot entry wrapper."
            );
        } else {
            println!("phase0_status: partial");
            println!(
                "phase0_note: image has a compact physical load span, but the current x86_64 \
                 entry is the Multiboot _start path. A direct HTTP Boot entry wrapper or \
                 Multiboot context synthesis is still required before jump."
            );
        }
        println!(
            "manifest_v1_candidate: kernel_load_addr={} entry_point={}",
            hex(self.load_addr),
            hex(self.entry_paddr)
        );
    }
}

pub fn inspect_elf(path: &Path) -> anyhow::Result<ElfImageReport> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    inspect_elf_bytes(path, &bytes)
}

pub fn write_flat_binary_from_elf(
    elf_path: &Path,
    report: &ElfImageReport,
    output_path: &Path,
) -> anyhow::Result<()> {
    let bytes =
        fs::read(elf_path).with_context(|| format!("failed to read {}", elf_path.display()))?;
    write_flat_binary_from_elf_bytes(elf_path, &bytes, report, output_path)
}

pub fn parse_hex_u64(value: &str) -> anyhow::Result<u64> {
    let value = value.trim();
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    u64::from_str_radix(value, 16).with_context(|| format!("invalid hex value `{value}`"))
}

pub fn validate_manifest_address(name: &str, actual: &str, expected: u64) -> anyhow::Result<()> {
    let actual_value = parse_hex_u64(actual)?;
    if actual_value != expected {
        bail!(
            "{name} `{actual}` does not match inspected ELF value {}",
            hex(expected)
        );
    }
    Ok(())
}

pub fn hex(value: u64) -> String {
    format!("{value:#x}")
}

fn inspect_elf_bytes(path: &Path, bytes: &[u8]) -> anyhow::Result<ElfImageReport> {
    let elf: object::read::elf::ElfFile64<'_, object::Endianness> =
        object::read::elf::ElfFile64::parse(bytes)
            .map_err(|e| anyhow!("failed to parse x86_64 ELF {}: {e}", path.display()))?;
    let endian = elf.endian();
    let entry: u64 = elf.elf_header().e_entry(endian);

    let mut load_segments = Vec::new();
    for (index, header) in elf.elf_program_headers().iter().enumerate() {
        if header.p_type(endian) != object::elf::PT_LOAD {
            continue;
        }
        let file_size = header.p_filesz(endian);
        let mem_size = header.p_memsz(endian);
        if mem_size == 0 {
            continue;
        }
        load_segments.push(SegmentInfo {
            index,
            vaddr: header.p_vaddr(endian),
            paddr: header.p_paddr(endian),
            file_size,
            mem_size,
        });
    }

    if load_segments.is_empty() {
        bail!("{} has no loadable ELF segments", path.display());
    }

    load_segments.sort_by_key(|segment| segment.paddr);

    let load_addr = load_segments[0].paddr;
    let image_file_end = max_segment_end(&load_segments, |segment| segment.file_size)?;
    let image_mem_end = max_segment_end(&load_segments, |segment| segment.mem_size)?;
    let first = &load_segments[0];
    let entry_offset = entry
        .checked_sub(first.vaddr)
        .ok_or_else(|| anyhow!("ELF entry is below first load segment virtual address"))?;
    let entry_paddr = first
        .paddr
        .checked_add(entry_offset)
        .ok_or_else(|| anyhow!("entry physical address overflows u64"))?;
    let httpboot_entry_symbol = find_symbol(&elf, "httpboot_entry");
    let manifest_entry_paddr = match httpboot_entry_symbol {
        Some(symbol) => first
            .paddr
            .checked_add(symbol.checked_sub(first.vaddr).ok_or_else(|| {
                anyhow!("httpboot_entry is below first load segment virtual address")
            })?)
            .ok_or_else(|| anyhow!("httpboot_entry physical address overflows u64"))?,
        None => entry_paddr,
    };

    Ok(ElfImageReport {
        entry,
        load_addr,
        image_file_size: image_file_end - load_addr,
        image_mem_size: image_mem_end - load_addr,
        entry_offset,
        entry_paddr: manifest_entry_paddr,
        start_symbol: find_symbol(&elf, "_start"),
        httpboot_entry_symbol,
        main_symbol: find_symbol(&elf, "main"),
        load_segments,
    })
}

fn write_flat_binary_from_elf_bytes(
    elf_path: &Path,
    bytes: &[u8],
    report: &ElfImageReport,
    output_path: &Path,
) -> anyhow::Result<()> {
    let elf: object::read::elf::ElfFile64<'_, object::Endianness> =
        object::read::elf::ElfFile64::parse(bytes)
            .map_err(|e| anyhow!("failed to parse x86_64 ELF {}: {e}", elf_path.display()))?;
    let endian = elf.endian();
    let image_len = usize::try_from(report.image_mem_size)
        .context("flat binary memory span does not fit usize")?;
    let mut image = vec![0; image_len];

    for header in elf.elf_program_headers() {
        if header.p_type(endian) != object::elf::PT_LOAD {
            continue;
        }
        let file_size: u64 = header.p_filesz(endian);
        if file_size == 0 {
            continue;
        }
        let paddr: u64 = header.p_paddr(endian);
        let offset = paddr.checked_sub(report.load_addr).ok_or_else(|| {
            anyhow!(
                "PT_LOAD segment paddr {} is below load addr {}",
                hex(paddr),
                hex(report.load_addr)
            )
        })?;
        let output_start = usize::try_from(offset).context("segment output offset overflow")?;
        let output_len = usize::try_from(file_size).context("segment file size overflow")?;
        let output_end = output_start
            .checked_add(output_len)
            .context("segment output range overflow")?;
        if output_end > image.len() {
            bail!(
                "PT_LOAD segment ending at {} exceeds flat image span {}",
                hex(paddr + file_size),
                hex(report.load_addr + report.image_mem_size)
            );
        }

        let file_offset: u64 = header.p_offset(endian);
        let input_start = usize::try_from(file_offset).context("segment file offset overflow")?;
        let input_end = input_start
            .checked_add(output_len)
            .context("segment input range overflow")?;
        let data = bytes
            .get(input_start..input_end)
            .ok_or_else(|| anyhow!("PT_LOAD segment data is outside {}", elf_path.display()))?;
        image[output_start..output_end].copy_from_slice(data);
    }

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(output_path, image)
        .with_context(|| format!("failed to write {}", output_path.display()))?;
    Ok(())
}

fn max_segment_end(
    segments: &[SegmentInfo],
    size: impl Fn(&SegmentInfo) -> u64,
) -> anyhow::Result<u64> {
    segments
        .iter()
        .map(|segment| {
            segment
                .paddr
                .checked_add(size(segment))
                .ok_or_else(|| anyhow!("load segment end address overflows u64"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .max()
        .context("load segment list should not be empty")
}

fn find_symbol(elf: &object::read::elf::ElfFile64<'_>, name: &str) -> Option<u64> {
    elf.symbols()
        .find(|symbol| symbol.name().ok() == Some(name))
        .map(|symbol| symbol.address())
}

fn option_hex(value: Option<u64>) -> String {
    value.map(hex).unwrap_or_else(|| "missing".to_string())
}

#[cfg(test)]
mod tests {
    use super::{hex, parse_hex_u64, validate_manifest_address};

    #[test]
    fn hex_formats_with_prefix() {
        assert_eq!(hex(0x20_0000), "0x200000");
    }

    #[test]
    fn parse_hex_accepts_optional_prefix() {
        assert_eq!(parse_hex_u64("0x200000").unwrap(), 0x20_0000);
        assert_eq!(parse_hex_u64("200000").unwrap(), 0x20_0000);
    }

    #[test]
    fn validate_manifest_address_rejects_mismatch() {
        let err = validate_manifest_address("entry_point", "0x1000", 0x2000).unwrap_err();
        assert!(err.to_string().contains("does not match"));
    }
}
