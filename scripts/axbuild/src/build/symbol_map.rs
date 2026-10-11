//! Build-time AXBT map generation.
//!
//! This module is intentionally host-only.  Its output is consumed by
//! `components/axbacktrace` in the target and never requires an ELF or DWARF
//! reader after the initramfs has been unpacked.

use std::{collections::HashMap, fs, path::Path};

use addr2line::Loader;
use anyhow::{Context, anyhow, ensure};
use object::{Architecture, Object, ObjectSection, ObjectSymbol, SymbolKind};
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 4] = b"AXBT";
const VERSION: u16 = 1;
const HEADER_LEN: usize = 32;
const RECORD_LEN: usize = 28;
const KERNEL_SYMBOL_MAGIC: &[u8; 4] = b"AXKS";
const KERNEL_SYMBOL_VERSION: u16 = 1;
const KERNEL_SYMBOL_HEADER_LEN: usize = 16;
const KERNEL_SYMBOL_RECORD_LEN: usize = 16;

/// Generates an AXBT v1 map beside a final kernel ELF.
pub(crate) fn generate_axbt_map(elf: &Path, output: &Path) -> anyhow::Result<()> {
    let bytes = fs::read(elf).with_context(|| format!("failed to read {}", elf.display()))?;
    let map = generate_axbt_map_bytes(&bytes, elf)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, map).with_context(|| format!("failed to write {}", output.display()))?;
    let kernel_symbols = generate_kernel_symbols_map(&bytes, elf)?;
    let kernel_symbols_path = output.with_extension("axks");
    fs::write(&kernel_symbols_path, kernel_symbols).with_context(|| {
        format!(
            "failed to write kernel symbol map {}",
            kernel_symbols_path.display()
        )
    })?;
    println!("[axbuild] AXBT map: {}", output.display());
    Ok(())
}

fn generate_kernel_symbols_map(bytes: &[u8], elf_path: &Path) -> anyhow::Result<Vec<u8>> {
    let file = object::File::parse(bytes)
        .with_context(|| format!("failed to parse ELF {}", elf_path.display()))?;
    let arch = architecture_tag(file.architecture())?;
    let build_id = file
        .build_id()
        .ok()
        .flatten()
        .map_or_else(|| Sha256::digest(bytes).to_vec(), ToOwned::to_owned);
    let mut symbols =
        file.symbols()
            .filter(|symbol| symbol.is_definition() && symbol.address() != 0)
            .filter_map(|symbol| {
                let name = symbol.name().ok()?.as_bytes().to_vec();
                if name.is_empty() {
                    return None;
                }
                let section_name = symbol
                    .section_index()
                    .and_then(|index| file.section_by_index(index).ok())
                    .and_then(|section| section.name().ok())
                    .unwrap_or("");
                let type_char =
                    match symbol.kind() {
                        SymbolKind::Text => {
                            if symbol.is_global() {
                                b'T'
                            } else {
                                b't'
                            }
                        }
                        SymbolKind::Data if section_name.starts_with(".bss") => {
                            if symbol.is_global() { b'B' } else { b'b' }
                        }
                        SymbolKind::Data if section_name.starts_with(".rodata") => {
                            if symbol.is_global() { b'R' } else { b'r' }
                        }
                        SymbolKind::Data => {
                            if symbol.is_global() {
                                b'D'
                            } else {
                                b'd'
                            }
                        }
                        _ => return None,
                    };
                Some((symbol.address(), name, type_char))
            })
            .collect::<Vec<_>>();
    symbols.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
    });
    symbols.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
    ensure!(
        !symbols.is_empty(),
        "ELF {} has no kallsyms-compatible symbols",
        elf_path.display()
    );
    let mut strings = StringTable::default();
    let mut records = Vec::with_capacity(symbols.len());
    for (address, name, type_char) in symbols {
        records.push((address, strings.push(&name)?, type_char));
    }
    ensure!(build_id.len() <= u8::MAX as usize, "build-id is too large");
    ensure!(
        strings.bytes.len() <= u32::MAX as usize,
        "AXKS string table is too large"
    );
    ensure!(
        records.len() <= u32::MAX as usize,
        "AXKS record table is too large"
    );

    let mut output = Vec::with_capacity(
        KERNEL_SYMBOL_HEADER_LEN
            + build_id.len()
            + records.len() * KERNEL_SYMBOL_RECORD_LEN
            + strings.bytes.len(),
    );
    output.extend_from_slice(KERNEL_SYMBOL_MAGIC);
    output.extend_from_slice(&KERNEL_SYMBOL_VERSION.to_le_bytes());
    output.push(arch);
    output.push(build_id.len() as u8);
    output.extend_from_slice(&(records.len() as u32).to_le_bytes());
    output.extend_from_slice(&(strings.bytes.len() as u32).to_le_bytes());
    output.extend_from_slice(&build_id);
    for (address, name, type_char) in records {
        output.extend_from_slice(&address.to_le_bytes());
        output.extend_from_slice(&name.to_le_bytes());
        output.push(type_char);
        output.extend_from_slice(&[0; 3]);
    }
    output.extend_from_slice(&strings.bytes);
    Ok(output)
}

fn generate_axbt_map_bytes(bytes: &[u8], elf_path: &Path) -> anyhow::Result<Vec<u8>> {
    let file = object::File::parse(bytes)
        .with_context(|| format!("failed to parse ELF {}", elf_path.display()))?;
    let arch = architecture_tag(file.architecture())?;
    let (text_start, text_end) = text_range(&file)?;
    // Prefer the ELF's stable GNU/OS build-id.  Minimal or locally linked
    // images may not carry one, so retain a deterministic content digest as
    // the fallback identity used by the target-side validation contract.
    let build_id = file
        .build_id()
        .ok()
        .flatten()
        .map_or_else(|| Sha256::digest(bytes).to_vec(), ToOwned::to_owned);

    let loader = Loader::new(elf_path).ok();
    let mut functions = file
        .symbols()
        .filter(|symbol| symbol.kind() == SymbolKind::Text)
        .filter_map(|symbol| {
            let start = symbol.address();
            let size = symbol.size();
            let name = symbol.name().ok().map(|name| {
                rustc_demangle::try_demangle(name)
                    .map_or_else(|_| name.to_string(), |name| name.to_string())
                    .into_bytes()
            })?;
            (start >= text_start && start < text_end && !name.is_empty())
                .then_some((start, size, name))
        })
        .collect::<Vec<_>>();
    functions.sort_by_key(|(start, ..)| *start);
    // ELF commonly carries aliases at one address.  Keep one record per start
    // address so the target binary-search table remains strictly ordered.
    functions.dedup_by_key(|entry| entry.0);
    ensure!(
        !functions.is_empty(),
        "ELF {} has no text symbols",
        elf_path.display()
    );

    let mut strings = StringTable::default();
    let mut records = Vec::with_capacity(functions.len());
    for (index, (start, size, name)) in functions.iter().enumerate() {
        let next_start = functions
            .get(index + 1)
            .map(|entry| entry.0)
            .unwrap_or(text_end);
        let end = start
            .checked_add(*size)
            .unwrap_or(next_start)
            .min(next_start)
            .min(text_end);
        if *start >= end {
            continue;
        }
        let name_offset = strings.push(name)?;
        let (file_offset, line) = loader
            .as_ref()
            .and_then(|loader| source_location(loader, *start))
            .map(|(file, line)| (strings.push(file.as_bytes()).ok(), line))
            .unwrap_or((None, 0));
        records.push((
            *start,
            end,
            name_offset,
            file_offset.unwrap_or(u32::MAX),
            line,
        ));
    }
    ensure!(
        !records.is_empty(),
        "ELF {} has no usable text symbols",
        elf_path.display()
    );
    ensure!(build_id.len() <= u8::MAX as usize, "build-id is too large");
    ensure!(
        strings.bytes.len() <= u32::MAX as usize,
        "AXBT string table is too large"
    );
    ensure!(
        records.len() <= u32::MAX as usize,
        "AXBT record table is too large"
    );

    let mut output = Vec::with_capacity(
        HEADER_LEN + build_id.len() + records.len() * RECORD_LEN + strings.bytes.len(),
    );
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&VERSION.to_le_bytes());
    output.push(arch);
    output.push(build_id.len() as u8);
    output.extend_from_slice(&text_start.to_le_bytes());
    output.extend_from_slice(&text_end.to_le_bytes());
    output.extend_from_slice(&(records.len() as u32).to_le_bytes());
    output.extend_from_slice(&(strings.bytes.len() as u32).to_le_bytes());
    output.extend_from_slice(&build_id);
    for (start, end, name, file, line) in records {
        output.extend_from_slice(&start.to_le_bytes());
        output.extend_from_slice(&end.to_le_bytes());
        output.extend_from_slice(&name.to_le_bytes());
        output.extend_from_slice(&file.to_le_bytes());
        output.extend_from_slice(&line.to_le_bytes());
    }
    output.extend_from_slice(&strings.bytes);
    Ok(output)
}

fn architecture_tag(architecture: Architecture) -> anyhow::Result<u8> {
    match architecture {
        Architecture::X86_64 => Ok(1),
        Architecture::Aarch64 => Ok(2),
        Architecture::Riscv64 => Ok(3),
        Architecture::Riscv32 => Ok(4),
        Architecture::LoongArch64 => Ok(5),
        other => Err(anyhow!("unsupported AXBT architecture: {other:?}")),
    }
}

fn text_range(file: &object::File<'_>) -> anyhow::Result<(u64, u64)> {
    let section = file
        .section_by_name(".text")
        .ok_or_else(|| anyhow!("ELF does not contain a .text section"))?;
    let start = section.address();
    let end = start
        .checked_add(section.size())
        .ok_or_else(|| anyhow!("ELF .text range overflows"))?;
    ensure!(start < end, "ELF .text section is empty");
    Ok((start, end))
}

#[derive(Default)]
struct StringTable {
    bytes: Vec<u8>,
    offsets: HashMap<Vec<u8>, u32>,
}

impl StringTable {
    fn push(&mut self, value: &[u8]) -> anyhow::Result<u32> {
        if let Some(&offset) = self.offsets.get(value) {
            return Ok(offset);
        }
        let offset = u32::try_from(self.bytes.len()).context("AXBT string offset overflows")?;
        ensure!(!value.contains(&0), "AXBT string contains NUL");
        std::str::from_utf8(value).context("AXBT string is not UTF-8")?;
        self.bytes.extend_from_slice(value);
        self.bytes.push(0);
        self.offsets.insert(value.to_vec(), offset);
        Ok(offset)
    }
}

fn source_location(loader: &Loader, address: u64) -> Option<(String, u32)> {
    let mut frames = loader.find_frames(address).ok()?;
    while let Ok(Some(frame)) = frames.next() {
        if let Some(location) = frame.location
            && let (Some(file), Some(line)) = (location.file, location.line)
        {
            return Some((file.to_string(), line));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_offsets_are_nul_terminated() {
        let mut strings = StringTable::default();
        assert_eq!(strings.push(b"kernel/src/lib.rs").unwrap(), 0);
        assert_eq!(strings.push(b"kernel/src/lib.rs").unwrap(), 0);
        assert_eq!(strings.bytes.last(), Some(&0));
    }
}
