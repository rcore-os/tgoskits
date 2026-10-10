#![no_std]

#[cfg(test)]
extern crate std;

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
use alloc::{boxed::Box, vec::Vec};
use core::{
    convert::TryFrom,
    fmt,
    ops::Range,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use ax_lazyinit::OnceLock;

static IP_RANGE: OnceLock<Range<usize>> = OnceLock::new();
static FP_RANGE: OnceLock<Range<usize>> = OnceLock::new();

/// Magic bytes at the beginning of an AXBT symbol map.
pub const AXBT_MAGIC: [u8; 4] = *b"AXBT";
/// Version of the binary symbol map format understood by this crate.
pub const AXBT_VERSION: u16 = 1;
/// Size of the fixed portion of an AXBT header, in bytes.
pub const AXBT_HEADER_LEN: usize = 32;
/// Size of one fixed-width function record, in bytes.
pub const AXBT_RECORD_LEN: usize = 28;

static SYMBOL_MAP: OnceLock<SymbolMap<'static>> = OnceLock::new();
static SYMBOL_MAP_INSTALLING: AtomicBool = AtomicBool::new(false);

const KERNEL_SYMBOL_MAGIC: [u8; 4] = *b"AXKS";
const KERNEL_SYMBOL_VERSION: u16 = 1;
const KERNEL_SYMBOL_HEADER_LEN: usize = 16;
const KERNEL_SYMBOL_RECORD_LEN: usize = 16;
static KERNEL_SYMBOL_MAP: OnceLock<KernelSymbolMap<'static>> = OnceLock::new();
#[cfg(feature = "alloc")]
static KERNEL_SYMBOL_MAP_INSTALLING: AtomicBool = AtomicBool::new(false);

#[cfg(target_arch = "x86_64")]
const TARGET_ARCH: &str = "x86_64";
#[cfg(target_arch = "aarch64")]
const TARGET_ARCH: &str = "aarch64";
#[cfg(target_arch = "riscv64")]
const TARGET_ARCH: &str = "riscv64";
#[cfg(target_arch = "riscv32")]
const TARGET_ARCH: &str = "riscv32";
#[cfg(target_arch = "loongarch64")]
const TARGET_ARCH: &str = "loongarch64";
#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64",
    target_arch = "riscv32",
    target_arch = "loongarch64"
)))]
const TARGET_ARCH: &str = "unknown";

const AXBT_ARCH_X86_64: u8 = 1;
const AXBT_ARCH_AARCH64: u8 = 2;
const AXBT_ARCH_RISCV64: u8 = 3;
const AXBT_ARCH_RISCV32: u8 = 4;
const AXBT_ARCH_LOONGARCH64: u8 = 5;

/// The architecture tag stored in an AXBT map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MapArchitecture {
    X86_64      = AXBT_ARCH_X86_64,
    Aarch64     = AXBT_ARCH_AARCH64,
    Riscv64     = AXBT_ARCH_RISCV64,
    Riscv32     = AXBT_ARCH_RISCV32,
    LoongArch64 = AXBT_ARCH_LOONGARCH64,
}

impl MapArchitecture {
    /// Returns the architecture tag used by this target.
    pub fn current() -> Self {
        cfg_if::cfg_if! {
            if #[cfg(target_arch = "x86_64")] {
                Self::X86_64
            } else if #[cfg(target_arch = "aarch64")] {
                Self::Aarch64
            } else if #[cfg(target_arch = "riscv64")] {
                Self::Riscv64
            } else if #[cfg(target_arch = "riscv32")] {
                Self::Riscv32
            } else if #[cfg(target_arch = "loongarch64")] {
                Self::LoongArch64
            } else {
                panic!("unsupported target architecture")
            }
        }
    }

    const fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            AXBT_ARCH_X86_64 => Some(Self::X86_64),
            AXBT_ARCH_AARCH64 => Some(Self::Aarch64),
            AXBT_ARCH_RISCV64 => Some(Self::Riscv64),
            AXBT_ARCH_RISCV32 => Some(Self::Riscv32),
            AXBT_ARCH_LOONGARCH64 => Some(Self::LoongArch64),
            _ => None,
        }
    }
}

/// Errors produced while validating an AXBT map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    TooShort,
    BadMagic,
    UnsupportedVersion(u16),
    UnsupportedArchitecture(u8),
    InvalidBuildId,
    InvalidTextRange,
    TextRangeMismatch,
    InvalidRecordCount,
    InvalidRecordRange,
    RecordsNotSorted,
    InvalidStringOffset,
    InvalidUtf8,
    ArchitectureMismatch,
    AlreadyInstalled,
}

/// A validated, zero-copy AXBT map.
///
/// The map owns no memory. Callers which install it globally must keep the
/// input bytes alive for the lifetime of the kernel (normally by placing them
/// in an initramfs allocation and leaking that allocation after validation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SymbolMap<'a> {
    bytes: &'a [u8],
    architecture: MapArchitecture,
    text_start: u64,
    text_end: u64,
    build_id_offset: usize,
    build_id_len: usize,
    records_offset: usize,
    record_count: usize,
    strings_offset: usize,
    strings_len: usize,
}

#[cfg(feature = "alloc")]
#[derive(Clone, Copy)]
struct SymbolMapLayout {
    architecture: MapArchitecture,
    build_id_offset: usize,
    build_id_len: usize,
    records_offset: usize,
    record_count: usize,
    strings_offset: usize,
    strings_len: usize,
    text_start: u64,
    text_end: u64,
}

/// One function and optional source location from a [`SymbolMap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Symbol<'a> {
    pub name: &'a str,
    pub file: Option<&'a str>,
    pub line: Option<u32>,
}

/// Symbol kinds emitted for Linux-compatible `/proc/kallsyms` consumers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum KernelSymbolKind {
    TextGlobal     = b'T',
    TextLocal      = b't',
    DataGlobal     = b'D',
    DataLocal      = b'd',
    BssGlobal      = b'B',
    BssLocal       = b'b',
    ReadOnlyGlobal = b'R',
    ReadOnlyLocal  = b'r',
}

impl KernelSymbolKind {
    const fn from_raw(raw: u8) -> Option<Self> {
        Some(match raw {
            b'T' => Self::TextGlobal,
            b't' => Self::TextLocal,
            b'D' => Self::DataGlobal,
            b'd' => Self::DataLocal,
            b'B' => Self::BssGlobal,
            b'b' => Self::BssLocal,
            b'R' => Self::ReadOnlyGlobal,
            b'r' => Self::ReadOnlyLocal,
            _ => return None,
        })
    }

    pub const fn as_char(self) -> char {
        self as u8 as char
    }
}

/// A validated, zero-copy kernel symbol table used by StarryOS procfs,
/// kprobe, and kmod paths.  It is kept separate from the function interval
/// table because data symbols do not have a meaningful instruction range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelSymbolMap<'a> {
    bytes: &'a [u8],
    architecture: MapArchitecture,
    build_id_offset: usize,
    build_id_len: usize,
    records_offset: usize,
    record_count: usize,
    strings_offset: usize,
    strings_len: usize,
}

/// One entry in a [`KernelSymbolMap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelSymbol<'a> {
    pub address: usize,
    pub name: &'a str,
    pub kind: KernelSymbolKind,
}

impl fmt::Display for Symbol<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;
        if let Some(file) = self.file {
            write!(f, " at {file}")?;
            if let Some(line) = self.line {
                write!(f, ":{line}")?;
            }
        }
        Ok(())
    }
}

impl<'a> SymbolMap<'a> {
    /// Parses and validates an AXBT map without allocating.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, MapError> {
        if bytes.len() < AXBT_HEADER_LEN {
            return Err(MapError::TooShort);
        }
        if bytes[..4] != AXBT_MAGIC {
            return Err(MapError::BadMagic);
        }
        let version = read_u16(bytes, 4).ok_or(MapError::TooShort)?;
        if version != AXBT_VERSION {
            return Err(MapError::UnsupportedVersion(version));
        }
        let architecture = MapArchitecture::from_raw(bytes[6])
            .ok_or(MapError::UnsupportedArchitecture(bytes[6]))?;
        let build_id_len = usize::from(bytes[7]);
        if build_id_len == 0 {
            return Err(MapError::InvalidBuildId);
        }
        let text_start = read_u64(bytes, 8).ok_or(MapError::TooShort)?;
        let text_end = read_u64(bytes, 16).ok_or(MapError::TooShort)?;
        if text_start >= text_end {
            return Err(MapError::InvalidTextRange);
        }
        if usize::try_from(text_start).is_err() || usize::try_from(text_end).is_err() {
            return Err(MapError::InvalidTextRange);
        }
        let record_count = usize::try_from(read_u32(bytes, 24).ok_or(MapError::TooShort)?)
            .map_err(|_| MapError::InvalidRecordCount)?;
        let strings_len = usize::try_from(read_u32(bytes, 28).ok_or(MapError::TooShort)?)
            .map_err(|_| MapError::InvalidRecordCount)?;
        let records_bytes = record_count
            .checked_mul(AXBT_RECORD_LEN)
            .ok_or(MapError::InvalidRecordCount)?;
        let records_offset = AXBT_HEADER_LEN
            .checked_add(build_id_len)
            .ok_or(MapError::InvalidRecordCount)?;
        let strings_offset = records_offset
            .checked_add(records_bytes)
            .ok_or(MapError::InvalidRecordCount)?;
        let end = strings_offset
            .checked_add(strings_len)
            .ok_or(MapError::InvalidRecordCount)?;
        if end != bytes.len() {
            return Err(MapError::InvalidRecordCount);
        }

        let map = Self {
            bytes,
            architecture,
            text_start,
            text_end,
            build_id_offset: AXBT_HEADER_LEN,
            build_id_len,
            records_offset,
            record_count,
            strings_offset,
            strings_len,
        };

        let mut previous_end = text_start;
        for index in 0..record_count {
            let record = map.raw_record(index).ok_or(MapError::InvalidRecordCount)?;
            if record.start < text_start || record.start >= record.end || record.end > text_end {
                return Err(MapError::InvalidRecordRange);
            }
            if record.start < previous_end {
                return Err(MapError::RecordsNotSorted);
            }
            previous_end = record.end;
            map.validate_string(record.name_offset)?;
            if record.file_offset != u32::MAX {
                map.validate_string(record.file_offset)?;
            }
        }
        Ok(map)
    }

    pub const fn architecture(&self) -> MapArchitecture {
        self.architecture
    }

    pub const fn text_range(&self) -> Range<usize> {
        self.text_start as usize..self.text_end as usize
    }

    /// Returns the opaque build identifier carried by this map.
    pub fn build_id(&self) -> &'a [u8] {
        &self.bytes[self.build_id_offset..self.build_id_offset + self.build_id_len]
    }

    pub const fn len(&self) -> usize {
        self.record_count
    }

    pub const fn is_empty(&self) -> bool {
        self.record_count == 0
    }

    /// Returns one validated function record for kernel symbol providers.
    pub fn symbol_at(&self, index: usize) -> Option<(usize, usize, Symbol<'a>)> {
        let record = self.raw_record(index)?;
        let name = self.string(record.name_offset)?;
        let file = if record.file_offset == u32::MAX {
            None
        } else {
            Some(self.string(record.file_offset)?)
        };
        Some((
            usize::try_from(record.start).ok()?,
            usize::try_from(record.end).ok()?,
            Symbol {
                name,
                file,
                line: (record.line != 0).then_some(record.line),
            },
        ))
    }

    /// Finds the function containing `address`.
    pub fn lookup(&self, address: usize) -> Option<Symbol<'a>> {
        let address = address as u64;
        if address < self.text_start || address >= self.text_end {
            return None;
        }
        let mut low = 0usize;
        let mut high = self.record_count;
        while low < high {
            let middle = low + (high - low) / 2;
            let record = self.raw_record(middle)?;
            if address < record.start {
                high = middle;
            } else if address >= record.end {
                low = middle + 1;
            } else {
                let name = self.string(record.name_offset)?;
                let file = if record.file_offset == u32::MAX {
                    None
                } else {
                    Some(self.string(record.file_offset)?)
                };
                return Some(Symbol {
                    name,
                    file,
                    line: (record.line != 0).then_some(record.line),
                });
            }
        }
        None
    }

    fn raw_record(&self, index: usize) -> Option<RawRecord> {
        if index >= self.record_count {
            return None;
        }
        let offset = self
            .records_offset
            .checked_add(index.checked_mul(AXBT_RECORD_LEN)?)?;
        Some(RawRecord {
            start: read_u64(self.bytes, offset)?,
            end: read_u64(self.bytes, offset.checked_add(8)?)?,
            name_offset: read_u32(self.bytes, offset.checked_add(16)?)?,
            file_offset: read_u32(self.bytes, offset.checked_add(20)?)?,
            line: read_u32(self.bytes, offset.checked_add(24)?)?,
        })
    }

    fn string(&self, offset: u32) -> Option<&'a str> {
        let offset = usize::try_from(offset).ok()?;
        if offset >= self.strings_len {
            return None;
        }
        let start = self.strings_offset.checked_add(offset)?;
        let bytes = &self.bytes[start..self.strings_offset + self.strings_len];
        let end = bytes.iter().position(|byte| *byte == 0)?;
        core::str::from_utf8(&bytes[..end]).ok()
    }

    fn validate_string(&self, offset: u32) -> Result<(), MapError> {
        let offset = usize::try_from(offset).map_err(|_| MapError::InvalidStringOffset)?;
        if offset >= self.strings_len {
            return Err(MapError::InvalidStringOffset);
        }
        let start = self
            .strings_offset
            .checked_add(offset)
            .ok_or(MapError::InvalidStringOffset)?;
        let bytes = &self.bytes[start..self.strings_offset + self.strings_len];
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(MapError::InvalidStringOffset)?;
        core::str::from_utf8(&bytes[..end]).map_err(|_| MapError::InvalidUtf8)?;
        Ok(())
    }

    #[cfg(feature = "alloc")]
    fn layout(&self) -> SymbolMapLayout {
        SymbolMapLayout {
            architecture: self.architecture,
            build_id_offset: self.build_id_offset,
            build_id_len: self.build_id_len,
            records_offset: self.records_offset,
            record_count: self.record_count,
            strings_offset: self.strings_offset,
            strings_len: self.strings_len,
            text_start: self.text_start,
            text_end: self.text_end,
        }
    }
}

#[cfg(feature = "alloc")]
impl SymbolMapLayout {
    fn attach<'a>(self, bytes: &'a [u8]) -> SymbolMap<'a> {
        SymbolMap {
            bytes,
            architecture: self.architecture,
            build_id_offset: self.build_id_offset,
            build_id_len: self.build_id_len,
            records_offset: self.records_offset,
            record_count: self.record_count,
            strings_offset: self.strings_offset,
            strings_len: self.strings_len,
            text_start: self.text_start,
            text_end: self.text_end,
        }
    }
}

impl<'a> KernelSymbolMap<'a> {
    /// Parses and validates an AXKS kernel symbol map without allocating.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, MapError> {
        if bytes.len() < KERNEL_SYMBOL_HEADER_LEN {
            return Err(MapError::TooShort);
        }
        if bytes[..4] != KERNEL_SYMBOL_MAGIC {
            return Err(MapError::BadMagic);
        }
        let version = read_u16(bytes, 4).ok_or(MapError::TooShort)?;
        if version != KERNEL_SYMBOL_VERSION {
            return Err(MapError::UnsupportedVersion(version));
        }
        let architecture = MapArchitecture::from_raw(bytes[6])
            .ok_or(MapError::UnsupportedArchitecture(bytes[6]))?;
        let build_id_len = usize::from(bytes[7]);
        if build_id_len == 0 {
            return Err(MapError::InvalidBuildId);
        }
        let record_count = usize::try_from(read_u32(bytes, 8).ok_or(MapError::TooShort)?)
            .map_err(|_| MapError::InvalidRecordCount)?;
        let strings_len = usize::try_from(read_u32(bytes, 12).ok_or(MapError::TooShort)?)
            .map_err(|_| MapError::InvalidRecordCount)?;
        let records_offset = KERNEL_SYMBOL_HEADER_LEN
            .checked_add(build_id_len)
            .ok_or(MapError::InvalidRecordCount)?;
        let strings_offset = records_offset
            .checked_add(
                record_count
                    .checked_mul(KERNEL_SYMBOL_RECORD_LEN)
                    .ok_or(MapError::InvalidRecordCount)?,
            )
            .ok_or(MapError::InvalidRecordCount)?;
        let end = strings_offset
            .checked_add(strings_len)
            .ok_or(MapError::InvalidRecordCount)?;
        if end != bytes.len() {
            return Err(MapError::InvalidRecordCount);
        }
        let map = Self {
            bytes,
            architecture,
            build_id_offset: KERNEL_SYMBOL_HEADER_LEN,
            build_id_len,
            records_offset,
            record_count,
            strings_offset,
            strings_len,
        };
        let mut previous = 0;
        for index in 0..record_count {
            let record = map.raw_record(index).ok_or(MapError::InvalidRecordCount)?;
            if index != 0 && record.address < previous {
                return Err(MapError::RecordsNotSorted);
            }
            previous = record.address;
            if KernelSymbolKind::from_raw(record.kind).is_none() {
                return Err(MapError::InvalidRecordRange);
            }
            map.validate_string(record.name_offset)?;
        }
        Ok(map)
    }

    pub const fn architecture(&self) -> MapArchitecture {
        self.architecture
    }

    pub fn build_id(&self) -> &'a [u8] {
        &self.bytes[self.build_id_offset..self.build_id_offset + self.build_id_len]
    }

    pub const fn len(&self) -> usize {
        self.record_count
    }

    pub const fn is_empty(&self) -> bool {
        self.record_count == 0
    }

    pub fn symbol_at(&self, index: usize) -> Option<KernelSymbol<'a>> {
        let record = self.raw_record(index)?;
        Some(KernelSymbol {
            address: usize::try_from(record.address).ok()?,
            name: self.string(record.name_offset)?,
            kind: KernelSymbolKind::from_raw(record.kind)?,
        })
    }

    #[cfg(feature = "alloc")]
    fn layout(&self) -> KernelSymbolMapLayout {
        KernelSymbolMapLayout {
            architecture: self.architecture,
            build_id_offset: self.build_id_offset,
            build_id_len: self.build_id_len,
            records_offset: self.records_offset,
            record_count: self.record_count,
            strings_offset: self.strings_offset,
            strings_len: self.strings_len,
        }
    }

    fn raw_record(&self, index: usize) -> Option<KernelRawRecord> {
        if index >= self.record_count {
            return None;
        }
        let offset = self
            .records_offset
            .checked_add(index.checked_mul(KERNEL_SYMBOL_RECORD_LEN)?)?;
        Some(KernelRawRecord {
            address: read_u64(self.bytes, offset)?,
            name_offset: read_u32(self.bytes, offset.checked_add(8)?)?,
            kind: *self.bytes.get(offset.checked_add(12)?)?,
        })
    }

    fn string(&self, offset: u32) -> Option<&'a str> {
        let offset = usize::try_from(offset).ok()?;
        if offset >= self.strings_len {
            return None;
        }
        let start = self.strings_offset.checked_add(offset)?;
        let bytes = &self.bytes[start..self.strings_offset + self.strings_len];
        let end = bytes.iter().position(|byte| *byte == 0)?;
        core::str::from_utf8(&bytes[..end]).ok()
    }

    fn validate_string(&self, offset: u32) -> Result<(), MapError> {
        self.string(offset)
            .map(|_| ())
            .ok_or(MapError::InvalidStringOffset)
    }
}

#[cfg(feature = "alloc")]
#[derive(Clone, Copy)]
struct KernelSymbolMapLayout {
    architecture: MapArchitecture,
    build_id_offset: usize,
    build_id_len: usize,
    records_offset: usize,
    record_count: usize,
    strings_offset: usize,
    strings_len: usize,
}

#[cfg(feature = "alloc")]
impl KernelSymbolMapLayout {
    fn attach<'a>(self, bytes: &'a [u8]) -> KernelSymbolMap<'a> {
        KernelSymbolMap {
            bytes,
            architecture: self.architecture,
            build_id_offset: self.build_id_offset,
            build_id_len: self.build_id_len,
            records_offset: self.records_offset,
            record_count: self.record_count,
            strings_offset: self.strings_offset,
            strings_len: self.strings_len,
        }
    }
}

#[derive(Clone, Copy)]
struct KernelRawRecord {
    address: u64,
    name_offset: u32,
    kind: u8,
}

#[derive(Clone, Copy)]
struct RawRecord {
    start: u64,
    end: u64,
    name_offset: u32,
    file_offset: u32,
    line: u32,
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?,
    ))
}

// Linkers may place a small alignment/padding tail after `_etext`. Keep the
// executable range check strict at the start while allowing that tail in a
// generated map.
const TEXT_RANGE_SLACK: usize = 4096;

fn validate_text_range(parsed: &SymbolMap<'_>) -> Result<(), MapError> {
    if let Some(text_range) = ip_range().as_ref() {
        let expected_end = text_range.end.saturating_add(TEXT_RANGE_SLACK);
        if (parsed.text_start as usize) < text_range.start
            || (parsed.text_end as usize) > expected_end
        {
            return Err(MapError::TextRangeMismatch);
        }
    }
    Ok(())
}

/// Installs the first valid, target-architecture AXBT map.
pub fn install_symbol_map(bytes: &'static [u8]) -> Result<(), MapError> {
    let parsed = SymbolMap::parse(bytes)?;
    if parsed.architecture != MapArchitecture::current() {
        return Err(MapError::ArchitectureMismatch);
    }
    validate_text_range(&parsed)?;
    reserve_install(&SYMBOL_MAP, &SYMBOL_MAP_INSTALLING)?;
    let _ = SYMBOL_MAP.call_once(|| parsed);
    Ok(())
}

/// Validates and installs an owned map, retaining its bytes for the lifetime
/// of the kernel. This is useful when an initramfs archive is reclaimed after
/// boot: the caller transfers ownership of the copied map to this function.
#[cfg(feature = "alloc")]
pub fn install_symbol_map_owned(bytes: Box<[u8]>) -> Result<(), MapError> {
    let layout = {
        let parsed = SymbolMap::parse(&bytes)?;
        if parsed.architecture != MapArchitecture::current() {
            return Err(MapError::ArchitectureMismatch);
        }
        validate_text_range(&parsed)?;
        parsed.layout()
    };
    reserve_install(&SYMBOL_MAP, &SYMBOL_MAP_INSTALLING)?;
    let bytes = Box::leak(bytes);
    let parsed = layout.attach(bytes);
    let _ = SYMBOL_MAP.call_once(|| parsed);
    Ok(())
}

fn reserve_install<T>(slot: &OnceLock<T>, installing: &AtomicBool) -> Result<(), MapError> {
    if slot.get().is_some() || installing.swap(true, Ordering::AcqRel) {
        Err(MapError::AlreadyInstalled)
    } else {
        Ok(())
    }
}

/// Returns the installed target symbol map, if one was installed.
pub fn symbol_map() -> Option<&'static SymbolMap<'static>> {
    SYMBOL_MAP.get()
}

/// Looks up an address in the installed target symbol map.
pub fn lookup_symbol(address: usize) -> Option<Symbol<'static>> {
    symbol_map()?.lookup(address)
}

/// Installs the first valid, target-architecture AXKS kernel symbol map.
#[cfg(feature = "alloc")]
pub fn install_kernel_symbol_map_owned(bytes: Box<[u8]>) -> Result<(), MapError> {
    let layout = {
        let parsed = KernelSymbolMap::parse(&bytes)?;
        if parsed.architecture != MapArchitecture::current() {
            return Err(MapError::ArchitectureMismatch);
        }
        parsed.layout()
    };
    reserve_install(&KERNEL_SYMBOL_MAP, &KERNEL_SYMBOL_MAP_INSTALLING)?;
    let bytes = Box::leak(bytes);
    let parsed = layout.attach(bytes);
    let _ = KERNEL_SYMBOL_MAP.call_once(|| parsed);
    Ok(())
}

/// Returns the installed Linux-compatible kernel symbol map.
pub fn kernel_symbol_map() -> Option<&'static KernelSymbolMap<'static>> {
    KERNEL_SYMBOL_MAP.get()
}

fn fmt_symbol(f: &mut fmt::Formatter<'_>, frame: &Frame) -> fmt::Result {
    if let Some(symbol) = lookup_symbol(frame.adjust_ip()) {
        write!(f, " symbol={symbol}")?;
    }
    Ok(())
}

/// Initializes the backtrace library.
pub fn init(ip_range: Range<usize>, fp_range: Range<usize>) {
    IP_RANGE.call_once(|| ip_range);
    FP_RANGE.call_once(|| fp_range);
}

/// Returns the initialized kernel instruction range.
pub fn ip_range() -> Option<Range<usize>> {
    IP_RANGE.get().cloned()
}

/// Returns the initialized kernel frame-pointer range.
pub fn fp_range() -> Option<Range<usize>> {
    FP_RANGE.get().cloned()
}

/// Represents a single stack frame in the unwound stack.
#[repr(C)]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub struct Frame {
    /// The frame pointer of the previous stack frame.
    pub fp: usize,
    /// The instruction pointer (program counter) after the function call.
    pub ip: usize,
}

impl Frame {
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    const OFFSET: usize = 0;
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    const OFFSET: usize = 1;

    fn read(fp: usize) -> Option<Self> {
        if fp == 0 || !fp.is_multiple_of(core::mem::align_of::<Frame>()) {
            return None;
        }

        Some(unsafe { (fp as *const Frame).sub(Self::OFFSET).read() })
    }

    // The stored IP is the return address (instruction after the call).
    // Subtracting the minimum instruction size gives an address that falls
    // within the calling function for the target AXBT interval lookup.
    #[cfg(target_arch = "x86_64")]
    pub fn adjust_ip(&self) -> usize {
        self.ip.wrapping_sub(1) // variable-length, 1 byte minimum
    }
    #[cfg(target_arch = "aarch64")]
    pub fn adjust_ip(&self) -> usize {
        self.ip.wrapping_sub(4) // fixed 4-byte instructions
    }
    #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
    pub fn adjust_ip(&self) -> usize {
        self.ip.wrapping_sub(2) // C extension: 2-byte minimum
    }
    #[cfg(target_arch = "loongarch64")]
    pub fn adjust_ip(&self) -> usize {
        self.ip.wrapping_sub(4) // fixed 4-byte instructions
    }
}

impl fmt::Display for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fp={:#x}, ip={:#x}", self.fp, self.ip)
    }
}

/// Walks an AAPCS-style frame-pointer chain without allocation.
///
/// `read` is injected by the caller so hard-IRQ users can perform no-fault page
/// table reads instead of dereferencing an untrusted frame pointer. `out[0]`
/// receives the sampled leaf PC; later entries are saved link registers.
pub fn walk_fp(
    pc: usize,
    mut fp: usize,
    ip_range: &Range<usize>,
    fp_range: &Range<usize>,
    read: impl Fn(usize) -> Option<usize>,
    out: &mut [u64],
) -> usize {
    const FP_TO_LR_OFFSET: usize = core::mem::size_of::<usize>();
    const FRAME_RECORD_SIZE: usize = 2 * FP_TO_LR_OFFSET;
    const MAX_FRAME_GAP: usize = 8 * 1024 * 1024;
    const MAX_STEP_FACTOR: usize = 4;

    let Some(leaf) = out.first_mut() else {
        return 0;
    };
    *leaf = pc as u64;
    let mut count = 1;
    let mut steps = 0;
    let max_steps = out.len().saturating_mul(MAX_STEP_FACTOR);

    while count < out.len() && steps < max_steps {
        steps += 1;
        let Some(record_end) = fp.checked_add(FRAME_RECORD_SIZE) else {
            break;
        };
        if !fp.is_multiple_of(core::mem::align_of::<usize>())
            || !fp_range.contains(&fp)
            || record_end > fp_range.end
        {
            break;
        }
        let Some(caller_fp) = read(fp) else {
            break;
        };
        let Some(lr_address) = fp.checked_add(FP_TO_LR_OFFSET) else {
            break;
        };
        let Some(lr) = read(lr_address) else { break };

        if ip_range.contains(&lr) {
            out[count] = lr as u64;
            count += 1;
        }
        if caller_fp == 0 || caller_fp <= fp || caller_fp.saturating_sub(fp) >= MAX_FRAME_GAP {
            break;
        }
        fp = caller_fp;
    }
    count
}

/// Capacity of the on-stack capture buffer. Matches the default `max_depth()`.
#[cfg(feature = "alloc")]
const CAPTURE_CAPACITY: usize = 32;

/// On-stack scratch buffer used during FP walking to avoid heap allocation
/// in the hot unwinding loop. Converted to `Box<[Frame]>` after the walk.
#[cfg(feature = "alloc")]
#[derive(Clone)]
struct CaptureBuf {
    frames: [Frame; CAPTURE_CAPACITY],
    len: usize,
}

#[cfg(feature = "alloc")]
impl CaptureBuf {
    const EMPTY: Self = Self {
        frames: [Frame { fp: 0, ip: 0 }; CAPTURE_CAPACITY],
        len: 0,
    };

    fn push(&mut self, frame: Frame) -> bool {
        if self.len < CAPTURE_CAPACITY {
            self.frames[self.len] = frame;
            self.len += 1;
            true
        } else {
            false
        }
    }

    /// Insert a frame at the front, shifting existing frames right.
    /// If the buffer is full, the last (deepest) frame is evicted to make room.
    fn insert_front(&mut self, frame: Frame) {
        let end = if self.len < CAPTURE_CAPACITY {
            self.len += 1;
            self.len
        } else {
            CAPTURE_CAPACITY // evict the deepest frame
        };
        self.frames.copy_within(0..end - 1, 1);
        self.frames[0] = frame;
    }

    fn first_mut(&mut self) -> Option<&mut Frame> {
        if self.len > 0 {
            Some(&mut self.frames[0])
        } else {
            None
        }
    }

    /// Convert to a heap-allocated boxed slice trimmed to the actual length.
    fn into_boxed_slice(self) -> Box<[Frame]> {
        self.frames[..self.len].into()
    }
}

/// Core frame pointer walking logic. Calls `callback` for each valid frame.
/// The callback returns `false` to stop unwinding (e.g., buffer full).
fn unwind_core(fp: usize, callback: impl FnMut(Frame) -> bool) -> bool {
    unwind_core_with_max_depth(fp, max_depth(), callback)
}

fn unwind_core_with_max_depth(
    mut fp: usize,
    max_depth: usize,
    mut callback: impl FnMut(Frame) -> bool,
) -> bool {
    let Some(fp_range) = FP_RANGE.get() else {
        return false;
    };

    let ip_range = IP_RANGE.get();
    let mut depth = 0;

    while fp_range.contains(&fp)
        && depth < max_depth
        && let Some(frame) = Frame::read(fp)
    {
        // Skip frames whose IP is outside the kernel text range.
        // We continue unwinding rather than stopping, as a corrupted
        // IP does not necessarily mean the FP chain is broken.
        // Skipped frames still count against the depth budget to prevent
        // infinite loops on corrupted FP chains with bad IPs.
        let next_fp = frame.fp;
        // Check FP progress before IP filtering: a bad IP can be skipped, but
        // a non-advancing FP would otherwise keep revisiting the same frame.
        if next_fp != 0 && next_fp <= fp {
            break;
        }

        if let Some(ip_range) = ip_range
            && !ip_range.contains(&frame.ip)
        {
            fp = next_fp;
            depth += 1;
            continue;
        }

        if !callback(frame) {
            break;
        }

        if let Some(large_stack_end) = fp.checked_add(8 * 1024 * 1024)
            && next_fp >= large_stack_end
        {
            break;
        }

        if next_fp == 0 {
            break;
        }

        fp = next_fp;
        depth += 1;
    }

    true
}

/// Unwind the stack from the given frame pointer.
#[cfg(feature = "alloc")]
pub fn unwind_stack(fp: usize) -> Vec<Frame> {
    let mut frames = Vec::new();
    unwind_core(fp, |frame| {
        frames.push(frame);
        true
    });
    frames
}

static MAX_DEPTH: AtomicUsize = AtomicUsize::new(32);

/// Sets the maximum depth for stack unwinding.
pub fn set_max_depth(depth: usize) {
    if depth > 0 {
        MAX_DEPTH.store(depth, Ordering::Relaxed);
    }
}
/// Returns the maximum depth for stack unwinding.
pub fn max_depth() -> usize {
    MAX_DEPTH.load(Ordering::Relaxed)
}

fn current_frame_pointer() -> Option<usize> {
    use core::arch::asm;

    let fp: usize;
    cfg_if::cfg_if! {
        if #[cfg(target_arch = "x86_64")] {
            unsafe { asm!("mov {ptr}, rbp", ptr = out(reg) fp) };
        } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
            unsafe { asm!("addi {ptr}, s0, 0", ptr = out(reg) fp) };
        } else if #[cfg(target_arch = "aarch64")] {
            unsafe { asm!("mov {ptr}, x29", ptr = out(reg) fp) };
        } else if #[cfg(target_arch = "loongarch64")] {
            unsafe { asm!("move {ptr}, $fp", ptr = out(reg) fp) };
        } else {
            return None;
        }
    }
    Some(fp)
}

/// An allocation-free, streaming stack backtrace.
///
/// Unlike [`Backtrace`], this type retains only the current frame pointer. Its
/// [`fmt::Display`] implementation walks and writes one frame at a time, so it
/// is suitable for panic and oops paths where the allocator may be unavailable
/// or already locked. An installed AXBT map is used directly by the target;
/// without one, output remains an address-only fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawBacktrace {
    fp: usize,
    kind: &'static str,
}

impl RawBacktrace {
    /// Captures the frame pointer without allocating or walking the stack.
    pub fn capture() -> Self {
        Self {
            fp: current_frame_pointer().unwrap_or(0),
            kind: "raw",
        }
    }

    /// Sets the machine-readable backtrace kind.
    pub fn kind(mut self, kind: &'static str) -> Self {
        self.kind = kind;
        self
    }
}

impl fmt::Display for RawBacktrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "BACKTRACE_BEGIN kind={} arch={} alloc=false map={}",
            self.kind,
            TARGET_ARCH,
            symbol_map().is_some()
        )?;

        match self.fp {
            0 => writeln!(f, "BT_ERROR unsupported")?,
            fp => {
                let mut index = 0;
                let mut write_error = None;
                let initialized = unwind_core(fp, |frame| {
                    if let Err(error) =
                        write!(f, "BT {index} ip={:#x} fp={:#x}", frame.ip, frame.fp)
                    {
                        write_error = Some(error);
                        return false;
                    }
                    if let Err(error) = fmt_symbol(f, &frame) {
                        write_error = Some(error);
                        return false;
                    }
                    if let Err(error) = writeln!(f) {
                        write_error = Some(error);
                        return false;
                    }
                    index += 1;
                    true
                });
                if let Some(error) = write_error {
                    return Err(error);
                }
                if !initialized {
                    writeln!(f, "BT_ERROR uninitialized")?;
                }
            }
        }

        writeln!(f, "BACKTRACE_END")
    }
}

/// Returns whether the backtrace feature is enabled.
pub const fn is_enabled() -> bool {
    cfg!(feature = "alloc")
}

#[allow(dead_code)]
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone)]
enum Inner {
    Unsupported,
    Disabled,
    #[cfg(feature = "alloc")]
    Captured(Box<[Frame]>),
}

/// A captured OS thread stack backtrace.
///
/// Internally stores frames as a `Box<[Frame]>` (trimmed to actual length).
/// Capture uses a stack-allocated scratch buffer so the FP walking loop
/// itself is allocation-free; the single `Box` allocation happens only after
/// the walk completes.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone)]
pub struct Backtrace {
    inner: Inner,
    kind: Option<&'static str>,
}

impl Backtrace {
    /// Capture the current thread's stack backtrace.
    pub fn capture() -> Self {
        #[cfg(not(feature = "alloc"))]
        return Self {
            inner: Inner::Disabled,
            kind: None,
        };

        #[cfg(feature = "alloc")]
        {
            let Some(fp) = current_frame_pointer() else {
                return Self {
                    inner: Inner::Unsupported,
                    kind: None,
                };
            };

            let mut buf = CaptureBuf::EMPTY;
            unwind_core(fp, |frame| buf.push(frame));

            core::hint::black_box(());

            Self {
                inner: Inner::Captured(buf.into_boxed_slice()),
                kind: None,
            }
        }
    }

    /// Capture the stack backtrace from a trap.
    ///
    /// - `fp`: frame pointer from the trap context
    /// - `ip`: faulting instruction pointer (the PC from the trap frame)
    /// - `ra`: return address (link register). On x86_64 this is always 0
    ///   since the return address is stored on the stack as part of the FP chain.
    #[allow(unused_variables)]
    pub fn capture_trap(fp: usize, ip: usize, ra: usize) -> Self {
        #[cfg(not(feature = "alloc"))]
        return Self {
            inner: Inner::Disabled,
            kind: None,
        };

        #[cfg(feature = "alloc")]
        {
            let mut buf = CaptureBuf::EMPTY;
            unwind_core(fp, |frame| buf.push(frame));

            // If the first unwound frame's IP is outside the kernel text,
            // it is likely the saved return address was not yet set (e.g.
            // leaf function fault). Replace it with the link register (ra)
            // only when ra is valid and within the kernel text range.
            // Note: on x86_64, ra=0 is always passed, so this branch
            // never fires for x86_64.
            if let Some(first) = buf.first_mut()
                && let Some(ip_range) = IP_RANGE.get()
                && !ip_range.contains(&first.ip)
                && ra != 0
                && ip_range.contains(&ra)
            {
                first.ip = ra;
            }

            buf.insert_front(Frame {
                fp,
                ip: ip.wrapping_add(1),
            });

            Self {
                inner: Inner::Captured(buf.into_boxed_slice()),
                kind: None,
            }
        }
    }

    /// Sets the backtrace kind for machine-parseable raw output via [`Display`].
    pub fn kind(mut self, kind: &'static str) -> Self {
        self.kind = Some(kind);
        self
    }
}

impl Backtrace {
    fn fmt_raw_block(&self, f: &mut fmt::Formatter<'_>, kind: &'static str) -> fmt::Result {
        let arch = TARGET_ARCH;

        writeln!(
            f,
            "BACKTRACE_BEGIN kind={} arch={} alloc={} map={}",
            kind,
            arch,
            cfg!(feature = "alloc"),
            symbol_map().is_some()
        )?;

        match &self.inner {
            Inner::Unsupported => {
                writeln!(f, "BT_ERROR unsupported")?;
            }
            Inner::Disabled => {
                if cfg!(feature = "alloc") {
                    writeln!(f, "BT_ERROR disabled")?;
                } else {
                    writeln!(f, "BT_ERROR requires_alloc")?;
                }
            }
            #[cfg(feature = "alloc")]
            Inner::Captured(frames) => {
                for (i, raw) in frames.iter().enumerate() {
                    write!(f, "BT {i} ip={:#x} fp={:#x}", raw.ip, raw.fp)?;
                    fmt_symbol(f, raw)?;
                    writeln!(f)?;
                }
            }
        }

        writeln!(f, "BACKTRACE_END")
    }
}

impl fmt::Display for Backtrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(kind) = self.kind {
            return self.fmt_raw_block(f, kind);
        }

        match &self.inner {
            Inner::Unsupported => {
                writeln!(f, "<unwinding unsupported>")
            }
            Inner::Disabled => {
                if cfg!(feature = "alloc") {
                    writeln!(f, "<backtrace disabled>")
                } else {
                    writeln!(f, "<backtrace requires alloc>")
                }
            }
            #[cfg(feature = "alloc")]
            Inner::Captured(frames) => {
                writeln!(f, "Backtrace:")?;
                if symbol_map().is_some() {
                    for (i, raw) in frames.iter().enumerate() {
                        write!(f, "{i:>4}: {raw}")?;
                        fmt_symbol(f, raw)?;
                        writeln!(f)?;
                    }
                    return Ok(());
                }
                for (i, raw) in frames.iter().enumerate() {
                    writeln!(f, "{i:>4}: {raw}")?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Debug for Backtrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
mod axbt_map_tests {
    use std::{format, vec::Vec};

    use super::*;

    fn put_u16(bytes: &mut Vec<u8>, value: u16) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn put_u64(bytes: &mut Vec<u8>, value: u64) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn valid_map() -> Vec<u8> {
        let strings = b"first\0src/main.rs\0second\0src/lib.rs\0";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&AXBT_MAGIC);
        put_u16(&mut bytes, AXBT_VERSION);
        bytes.push(MapArchitecture::current() as u8);
        bytes.push(3); // build-id length
        put_u64(&mut bytes, 0x1000);
        put_u64(&mut bytes, 0x2000);
        put_u32(&mut bytes, 2);
        put_u32(&mut bytes, strings.len() as u32);
        bytes.extend_from_slice(b"id1");
        // start, end, name offset, file offset, line
        put_u64(&mut bytes, 0x1000);
        put_u64(&mut bytes, 0x1100);
        put_u32(&mut bytes, 0);
        put_u32(&mut bytes, 6);
        put_u32(&mut bytes, 12);
        put_u64(&mut bytes, 0x1100);
        put_u64(&mut bytes, 0x1200);
        put_u32(&mut bytes, 18);
        put_u32(&mut bytes, 25);
        put_u32(&mut bytes, 34);
        bytes.extend_from_slice(strings);
        bytes
    }

    #[test]
    fn parses_build_id_and_lookup_boundaries() {
        let bytes = valid_map();
        let map = SymbolMap::parse(&bytes).unwrap();
        assert_eq!(map.build_id(), b"id1");
        assert_eq!(map.len(), 2);
        assert_eq!(map.lookup(0x0fff), None);
        assert_eq!(
            map.lookup(0x1000),
            Some(Symbol {
                name: "first",
                file: Some("src/main.rs"),
                line: Some(12),
            })
        );
        assert_eq!(map.lookup(0x10ff).unwrap().name, "first");
        assert_eq!(map.lookup(0x1100).unwrap().name, "second");
        assert_eq!(map.lookup(0x1200), None);
        assert_eq!(
            format!("{}", map.lookup(0x1000).unwrap()),
            "first at src/main.rs:12"
        );
    }

    #[test]
    fn parses_kernel_symbol_types() {
        let strings = b"text\0data\0";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"AXKS");
        put_u16(&mut bytes, 1);
        bytes.push(MapArchitecture::current() as u8);
        bytes.push(2);
        put_u32(&mut bytes, 2);
        put_u32(&mut bytes, strings.len() as u32);
        bytes.extend_from_slice(b"id");
        put_u64(&mut bytes, 0x1000);
        put_u32(&mut bytes, 0);
        bytes.extend_from_slice(b"T\0\0\0");
        put_u64(&mut bytes, 0x2000);
        put_u32(&mut bytes, 5);
        bytes.extend_from_slice(b"D\0\0\0");
        bytes.extend_from_slice(strings);

        let map = KernelSymbolMap::parse(&bytes).unwrap();
        assert_eq!(map.symbol_at(0).unwrap().kind, KernelSymbolKind::TextGlobal);
        assert_eq!(map.symbol_at(1).unwrap().name, "data");
        assert_eq!(map.symbol_at(1).unwrap().kind.as_char(), 'D');
    }

    #[test]
    fn rejects_malformed_header_and_record_bounds() {
        assert_eq!(SymbolMap::parse(&[]), Err(MapError::TooShort));
        let mut bytes = valid_map();
        bytes[0] = b'X';
        assert_eq!(SymbolMap::parse(&bytes), Err(MapError::BadMagic));
        let mut bytes = valid_map();
        bytes[4] = 2;
        assert_eq!(
            SymbolMap::parse(&bytes),
            Err(MapError::UnsupportedVersion(2))
        );
        let mut bytes = valid_map();
        bytes[6] = 0xff;
        assert_eq!(
            SymbolMap::parse(&bytes),
            Err(MapError::UnsupportedArchitecture(0xff))
        );
        let mut bytes = valid_map();
        bytes[7] = 0;
        assert_eq!(SymbolMap::parse(&bytes), Err(MapError::InvalidBuildId));
        let mut bytes = valid_map();
        // First function end exceeds the declared text range.
        bytes[43..51].copy_from_slice(&0x3000u64.to_le_bytes());
        assert_eq!(SymbolMap::parse(&bytes), Err(MapError::InvalidRecordRange));
        let mut bytes = valid_map();
        bytes.push(0);
        assert_eq!(SymbolMap::parse(&bytes), Err(MapError::InvalidRecordCount));
    }

    #[test]
    fn rejects_unsorted_and_invalid_strings() {
        let mut bytes = valid_map();
        // Second function starts before the first one ends.
        bytes[63..71].copy_from_slice(&0x1001u64.to_le_bytes());
        assert_eq!(SymbolMap::parse(&bytes), Err(MapError::RecordsNotSorted));

        let mut bytes = valid_map();
        // First name points outside the string table.
        bytes[51..55].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(SymbolMap::parse(&bytes), Err(MapError::InvalidStringOffset));

        let mut bytes = valid_map();
        let strings_offset = AXBT_HEADER_LEN + 3 + 2 * AXBT_RECORD_LEN;
        bytes[strings_offset] = 0xff;
        assert_eq!(SymbolMap::parse(&bytes), Err(MapError::InvalidUtf8));
    }
}

#[cfg(all(test, feature = "alloc"))]
mod tests {
    use alloc::{boxed::Box, format, vec::Vec};

    use super::*;

    fn init_for_tests() {
        init(0..usize::MAX, 0..usize::MAX);
    }

    fn boxed_frame_chain(ips: &[usize]) -> (Box<[Frame]>, usize) {
        let mut frames = ips
            .iter()
            .map(|&ip| Frame { fp: 0, ip })
            .collect::<Vec<_>>()
            .into_boxed_slice();

        let ptr = frames.as_mut_ptr();
        for i in 0..frames.len() {
            let next_fp = if i + 1 < frames.len() {
                unsafe { ptr.add(i + 1) as usize }
            } else {
                0
            };
            frames[i].fp = next_fp;
        }
        (frames, ptr as usize)
    }

    // --- CaptureBuf internal tests ---

    #[test]
    fn capture_buf_push_and_insert() {
        let mut buf = CaptureBuf::EMPTY;
        assert!(buf.push(Frame { fp: 1, ip: 0x10 }));
        assert!(buf.push(Frame { fp: 2, ip: 0x20 }));
        assert_eq!(buf.len, 2);

        buf.insert_front(Frame { fp: 0, ip: 0x05 });
        assert_eq!(buf.len, 3);
        assert_eq!(
            &*buf.clone().into_boxed_slice(),
            &[
                Frame { fp: 0, ip: 0x05 },
                Frame { fp: 1, ip: 0x10 },
                Frame { fp: 2, ip: 0x20 }
            ]
        );
    }

    #[test]
    fn capture_buf_overflow_evicts_deepest() {
        let mut buf = CaptureBuf::EMPTY;
        for i in 0..CAPTURE_CAPACITY {
            assert!(buf.push(Frame { fp: i, ip: i }));
        }
        assert!(!buf.push(Frame { fp: 0, ip: 0 })); // full
        buf.insert_front(Frame { fp: 99, ip: 0x99 });
        assert_eq!(buf.len, CAPTURE_CAPACITY);
        let boxed = buf.into_boxed_slice();
        assert_eq!(boxed[0], Frame { fp: 99, ip: 0x99 });
        assert_eq!(boxed.len(), CAPTURE_CAPACITY);
    }

    #[test]
    fn into_boxed_slice_trims_to_len() {
        let mut buf = CaptureBuf::EMPTY;
        buf.push(Frame { fp: 1, ip: 0x10 });
        buf.push(Frame { fp: 2, ip: 0x20 });
        let boxed = buf.into_boxed_slice();
        assert_eq!(boxed.len(), 2);
        assert_eq!(boxed[0], Frame { fp: 1, ip: 0x10 });
    }

    // --- Frame::read / unwind_core internal tests ---

    #[test]
    fn unwind_stack_collects_fake_frames() {
        init_for_tests();
        let (frames, start_fp) = boxed_frame_chain(&[0x1111, 0x2222, 0x3333]);
        let out = unwind_stack(start_fp);
        assert_eq!(out, frames.as_ref());
    }

    #[test]
    fn raw_backtrace_streams_frames_without_captured_storage() {
        init_for_tests();
        let (_frames, start_fp) = boxed_frame_chain(&[0x1111, 0x2222, 0x3333]);
        let raw = RawBacktrace {
            fp: start_fp,
            kind: "panic",
        };

        let output = format!("{raw}");
        assert!(output.contains("BACKTRACE_BEGIN kind=panic"));
        assert!(output.contains("alloc=false map=false"));
        assert!(output.contains("BT 0 ip=0x1111"));
        assert!(output.contains("BT 2 ip=0x3333"));
        assert!(output.ends_with("BACKTRACE_END\n"));
        assert_eq!(
            core::mem::size_of::<RawBacktrace>(),
            3 * core::mem::size_of::<usize>()
        );
    }

    #[test]
    fn unwind_core_callback_stop_early() {
        init_for_tests();
        let (_chain, start_fp) = boxed_frame_chain(&[0x1, 0x2, 0x3, 0x4, 0x5]);
        let mut count = 0;
        unwind_core(start_fp, |_| {
            count += 1;
            count < 3
        });
        assert_eq!(count, 3);
    }

    #[test]
    fn unwind_stack_stops_on_non_advancing_frame_pointer() {
        init_for_tests();
        let mut frames = [Frame { fp: 0, ip: 0x1111 }, Frame { fp: 0, ip: 0x2222 }];
        let base = frames.as_mut_ptr();
        frames[0].fp = unsafe { base.add(1) as usize };
        frames[1].fp = base as usize;

        let out = unwind_stack(base as usize);
        assert_eq!(out, [frames[0]]);
    }

    #[test]
    fn frame_read_rejects_null_and_misaligned() {
        assert!(Frame::read(0).is_none());
        assert!(Frame::read(1).is_none());
        assert!(Frame::read(3).is_none());
    }

    // --- capture_trap with Inner::Captured verification ---

    #[test]
    fn capture_trap_ra_not_substituted_with_wide_range() {
        init_for_tests();
        let (_chain, start_fp) = boxed_frame_chain(&[0xDEAD]);
        let bt = Backtrace::capture_trap(start_fp, 0x1000, 0xBEEF);
        let Inner::Captured(frames) = &bt.inner else {
            panic!("expected Captured")
        };
        assert_eq!(frames[0].ip, 0x1001);
        assert_eq!(frames[1].ip, 0xDEAD); // not replaced by ra
    }

    // --- Stress tests ---

    /// Build a chain that fills the buffer to exactly CAPTURE_CAPACITY.
    /// Then unwind and verify every frame is collected.
    #[test]
    fn stress_fill_buffer_exactly() {
        init_for_tests();
        let ips: Vec<usize> = (0..CAPTURE_CAPACITY).map(|i| 0xA000 + i).collect();
        let (chain, start_fp) = boxed_frame_chain(&ips);
        let out = unwind_stack(start_fp);
        assert_eq!(out.len(), CAPTURE_CAPACITY);
        assert_eq!(out.as_slice(), chain.as_ref());
    }

    /// Build a chain with CAPTURE_CAPACITY - 1 frames, then capture_trap.
    /// The trap frame is inserted at front, total = CAPTURE_CAPACITY, no eviction.
    #[test]
    fn stress_trap_near_capacity() {
        init_for_tests();
        let n = CAPTURE_CAPACITY - 1;
        let ips: Vec<usize> = (0..n).map(|i| 0xB000 + i).collect();
        let (_chain, start_fp) = boxed_frame_chain(&ips);

        let bt = Backtrace::capture_trap(start_fp, 0xC000, 0);
        let Inner::Captured(frames) = &bt.inner else {
            panic!("expected Captured")
        };
        assert_eq!(frames.len(), CAPTURE_CAPACITY);
        // Trap frame is at front with ip = 0xC000 + 1
        assert_eq!(frames[0].ip, 0xC001);
        // Remaining frames follow
        for (i, f) in frames[1..].iter().enumerate() {
            assert_eq!(f.ip, 0xB000 + i);
        }
    }

    /// Build a chain with CAPTURE_CAPACITY frames, then capture_trap.
    /// The trap insert_front evicts the deepest frame.
    #[test]
    fn stress_trap_overflow_evicts_deepest() {
        init_for_tests();
        let ips: Vec<usize> = (0..CAPTURE_CAPACITY).map(|i| 0xD000 + i).collect();
        let (_chain, start_fp) = boxed_frame_chain(&ips);

        let bt = Backtrace::capture_trap(start_fp, 0xE000, 0);
        let Inner::Captured(frames) = &bt.inner else {
            panic!("expected Captured")
        };
        assert_eq!(frames.len(), CAPTURE_CAPACITY);
        // Trap frame at front
        assert_eq!(frames[0].ip, 0xE001);
        // The first CAPTURE_CAPACITY - 1 unwound frames remain
        for (i, f) in frames[1..].iter().enumerate() {
            assert_eq!(f.ip, 0xD000 + i);
        }
        // The deepest frame (0xD000 + CAPTURE_CAPACITY - 1) was evicted
    }

    /// Build a chain deeper than max_depth and verify truncation.
    #[test]
    fn stress_deep_chain_truncation() {
        init_for_tests();
        let ips: Vec<usize> = (0..64).map(|i| 0xF000 + i).collect();
        let (chain, start_fp) = boxed_frame_chain(&ips);

        let mut out = Vec::new();
        unwind_core_with_max_depth(start_fp, 16, |frame| {
            out.push(frame);
            true
        });
        assert_eq!(out.len(), 16);
        // Only the first 16 frames should be collected
        assert_eq!(out.as_slice(), &chain[..16]);
    }

    /// Repeatedly clone a Backtrace and verify equality.
    #[test]
    fn stress_repeated_clone() {
        init_for_tests();
        let (chain, start_fp) = boxed_frame_chain(&[0x800, 0x900, 0xA00]);
        let original = Backtrace::capture_trap(start_fp, 0xB00, 0);

        for _ in 0..200 {
            let cloned = original.clone();
            assert_eq!(cloned, original);
        }
        let _ = &chain;
    }

    /// Verify the layout used when reading native stack frame records.
    #[test]
    fn frame_layout_matches_native_stack_records() {
        // Frame is #[repr(C)] with two usize fields
        assert_eq!(
            core::mem::size_of::<Frame>(),
            2 * core::mem::size_of::<usize>()
        );
        assert_eq!(
            core::mem::align_of::<Frame>(),
            core::mem::align_of::<usize>()
        );
    }

    /// Verify Frame alignment and that misaligned pointers are rejected.
    #[test]
    fn stress_frame_alignment() {
        let align = core::mem::align_of::<Frame>();
        assert!(align > 0);
        assert!(align.is_power_of_two());

        // All valid FP values must be multiples of the alignment
        for offset in 1..align {
            assert!(
                Frame::read(offset).is_none(),
                "misaligned {offset} should fail"
            );
        }
        // Zero is always rejected
        assert!(Frame::read(0).is_none());
    }

    #[test]
    fn injected_fp_walker_captures_leaf_and_callers() {
        let (_chain, fp) = boxed_frame_chain(&[0x1110, 0x2220, 0x3330]);
        let mut out = [0; 8];
        let count = walk_fp(
            0x1000,
            fp,
            &(1..usize::MAX),
            &(fp..usize::MAX),
            |address| Some(unsafe { *(address as *const usize) }),
            &mut out,
        );
        assert_eq!(&out[..count], &[0x1000, 0x1110, 0x2220, 0x3330]);
    }

    #[test]
    fn injected_fp_walker_rejects_overflow_before_read() {
        let mut out = [0; 4];
        let count = walk_fp(
            0x1000,
            usize::MAX - 7,
            &(1..usize::MAX),
            &(0..usize::MAX),
            |_| panic!("overflowing frame must not be read"),
            &mut out,
        );
        assert_eq!(count, 1);
        assert_eq!(out[0], 0x1000);
    }
}
