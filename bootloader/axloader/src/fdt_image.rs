//! Bounded device-tree input for firmware loading and boot-resource checks.

use alloc::vec::Vec;
use core::{ffi::CStr, ops::Range};

use fdt_raw::{
    Token,
    data::{Bytes, Reader},
};
use thiserror::Error;
pub use uefi_raw::table::boot::{MemoryDescriptor, MemoryType};

/// Bytes needed to read a version-17 DTB header before mapping the full blob.
pub const HEADER_BYTES: usize = 40;
/// Maximum blob size, including any capacity introduced by firmware fixups.
pub const MAX_DTB_BYTES: usize = 16 * 1024 * 1024;
const MAX_DEPTH: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// A static physical range that the firmware owner must prove is protected.
pub struct MemoryReservation {
    pub address: u64,
    pub size: u64,
    /// Requires a range the OS cannot use for normal memory mappings.
    pub no_map: bool,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum DtbError {
    #[error("invalid DTB header")]
    Header,
    #[error("invalid DTB block range")]
    BlockRange,
    #[error("invalid DTB structure")]
    Structure,
    #[error("missing or invalid RISC-V timebase")]
    Timebase,
    #[error("invalid DTB memory reservation")]
    Reservation,
    #[error("unsupported DTB reservation encoding")]
    UnsupportedReservation,
    #[error("EFI memory map does not protect DTB reservation {0:?}")]
    UnprotectedReservation(MemoryReservation),
    #[error("DTB validation allocation failed")]
    OutOfResources,
}

/// Checks a readable header and returns the bounded full-blob size.
///
/// This does not prove the returned byte range is readable or validate its
/// blocks. Firmware callers must check their memory map before forming a slice.
pub fn header_total_size(bytes: &[u8]) -> Result<usize, DtbError> {
    let header = fdt_raw::Header::from_bytes(bytes).map_err(|_| DtbError::Header)?;
    let total = header.totalsize as usize;
    if !(HEADER_BYTES..=MAX_DTB_BYTES).contains(&total)
        || header.version < 17
        || header.last_comp_version > 17
        || header.last_comp_version > header.version
    {
        return Err(DtbError::Header);
    }
    Ok(total)
}

/// Validated boot input borrowing at most 16 MiB and 16 nested node levels.
///
/// This validates binary framing, the RISC-V timebase and static reservations;
/// it does not validate every device binding or certify the tree fits hardware.
pub struct ValidatedFdt<'a> {
    bytes: &'a [u8],
    reservations: Vec<MemoryReservation>,
}

impl<'a> ValidatedFdt<'a> {
    /// Validates all token/name/property bounds before exposing boot fields.
    /// Bytes after `totalsize` are spare buffer capacity and are never parsed.
    /// Dynamic requests are checked but have no physical range before OS
    /// allocation, so they are excluded from static reservation checks. Translated
    /// reserved-memory mappings are rejected.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, DtbError> {
        let total = header_total_size(bytes)?;
        let bytes = bytes.get(..total).ok_or(DtbError::BlockRange)?;
        let header = fdt_raw::Header::from_bytes(bytes).map_err(|_| DtbError::Header)?;
        let structure = block_range(header.off_dt_struct, header.size_dt_struct, total, 4)?;
        let strings = block_range(header.off_dt_strings, header.size_dt_strings, total, 1)?;
        if structure.is_empty() || overlap(&structure, &strings) {
            return Err(DtbError::BlockRange);
        }
        let mut reservations = Vec::new();
        let reserve_start = header.off_mem_rsvmap as usize;
        if reserve_start < HEADER_BYTES
            || reserve_start >= total
            || !reserve_start.is_multiple_of(8)
        {
            return Err(DtbError::BlockRange);
        }
        let mut reserve_reader = Bytes::new(&bytes[reserve_start..]).reader();
        loop {
            let address = reserve_reader.read_u64().ok_or(DtbError::Reservation)?;
            let size = reserve_reader.read_u64().ok_or(DtbError::Reservation)?;
            if address == 0 && size == 0 {
                break;
            }
            if size != 0 {
                append_reservation(&mut reservations, address, size, false)?;
            }
        }
        let reserve_end = reserve_start + reserve_reader.position();
        if overlap(&(reserve_start..reserve_end), &structure)
            || overlap(&(reserve_start..reserve_end), &strings)
        {
            return Err(DtbError::BlockRange);
        }
        validate_structure(&bytes[structure], &bytes[strings], &mut reservations)?;
        Ok(Self {
            bytes,
            reservations,
        })
    }

    /// Returns exactly the validated blob, excluding spare buffer capacity.
    pub fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }
    pub fn total_size(&self) -> usize {
        self.bytes.len()
    }
    /// Checks every static reservation against the firmware memory map.
    /// Use a snapshot taken before allocating the external DTB, so that the
    /// allocation cannot make an unprotected range appear protected.
    pub fn validate_uefi_reservations<'m>(
        &self,
        map: impl Iterator<Item = &'m MemoryDescriptor> + Clone,
    ) -> Result<(), DtbError> {
        for reservation in &self.reservations {
            if !efi_memory_covers(map.clone(), reservation.address, reservation.size, |kind| {
                if reservation.no_map {
                    kind == MemoryType::RESERVED
                } else {
                    matches!(
                        kind,
                        MemoryType::RESERVED
                            // DTSpec 3.5.4 uses Boot Services Data for ordinary
                            // static reservations; the kernel preserves them
                            // from the FDT before reclaiming boot memory.
                            | MemoryType::BOOT_SERVICES_DATA
                            | MemoryType::RUNTIME_SERVICES_CODE
                            | MemoryType::RUNTIME_SERVICES_DATA
                            | MemoryType::UNUSABLE
                            | MemoryType::ACPI_RECLAIM
                            | MemoryType::ACPI_NON_VOLATILE
                    )
                }
            }) {
                return Err(DtbError::UnprotectedReservation(*reservation));
            }
        }
        Ok(())
    }
}

/// Proves complete physical coverage, including adjacent, unordered descriptors.
/// Overflowing descriptors cannot establish coverage. This checks backing only;
/// the caller still owns the pointer lifetime and chooses admissible EFI types.
pub fn efi_memory_covers<'m>(
    map: impl Iterator<Item = &'m MemoryDescriptor> + Clone,
    start: u64,
    length: u64,
    accepts: impl Fn(MemoryType) -> bool,
) -> bool {
    let Some(end) = start.checked_add(length) else {
        return false;
    };
    let mut cursor = start;
    while cursor < end {
        let next = map
            .clone()
            .filter(|entry| accepts(entry.ty) && entry.phys_start <= cursor)
            .filter_map(|entry| {
                entry
                    .page_count
                    .checked_mul(4096)
                    .and_then(|size| entry.phys_start.checked_add(size))
            })
            .filter(|limit| *limit > cursor)
            .max();
        let Some(next) = next else { return false };
        cursor = next;
    }
    true
}

fn validate_structure(
    structure: &[u8],
    strings: &[u8],
    reservations: &mut Vec<MemoryReservation>,
) -> Result<u32, DtbError> {
    let mut reader = Bytes::new(structure).reader();
    let mut stack = [NodeState::default(); MAX_DEPTH];
    let mut depth = 0usize;
    let mut seen_root = false;
    let mut seen_cpus = false;
    let mut seen_reserved_memory = false;
    let mut timebase = None;
    loop {
        match reader.read_token().map_err(|_| DtbError::Structure)? {
            Token::BeginNode => {
                if depth == MAX_DEPTH || (depth == 0 && seen_root) {
                    return Err(DtbError::Structure);
                }
                let name = read_name(&mut reader)?;
                if (depth == 0 && !name.is_empty())
                    || (depth != 0 && (name.is_empty() || name.contains('/')))
                {
                    return Err(DtbError::Structure);
                }
                let parent = depth.checked_sub(1).map(|index| stack[index]);
                if let Some(index) = depth.checked_sub(1) {
                    stack[index].has_children = true;
                }
                let kind = match (depth, name, parent.map(|parent| parent.kind)) {
                    (1, "cpus", _) => NodeKind::Cpus,
                    (1, "reserved-memory", _) => NodeKind::ReservedMemory,
                    (_, _, Some(NodeKind::ReservedMemory)) => NodeKind::Reservation,
                    (_, _, Some(NodeKind::Reservation)) => {
                        return Err(DtbError::UnsupportedReservation);
                    }
                    _ => NodeKind::Other,
                };
                let mut node = NodeState {
                    kind,
                    ..NodeState::default()
                };
                if node.kind == NodeKind::Cpus && core::mem::replace(&mut seen_cpus, true)
                    || node.kind == NodeKind::ReservedMemory
                        && core::mem::replace(&mut seen_reserved_memory, true)
                {
                    return Err(DtbError::Structure);
                }
                if node.kind == NodeKind::Reservation {
                    let parent = parent.ok_or(DtbError::Structure)?;
                    if !parent.ranges
                        || !parent.address_declared
                        || !parent.size_declared
                        || !matches!(parent.address_cells, 1 | 2)
                        || !matches!(parent.size_cells, 1 | 2)
                    {
                        return Err(DtbError::UnsupportedReservation);
                    }
                    node.address_cells = parent.address_cells;
                    node.size_cells = parent.size_cells;
                }
                stack[depth] = node;
                depth += 1;
                seen_root = true;
            }
            Token::Prop => {
                let index = depth.checked_sub(1).ok_or(DtbError::Structure)?;
                let node = &mut stack[index];
                if node.has_children {
                    return Err(DtbError::Structure);
                }
                let length = reader.read_u32().ok_or(DtbError::Structure)? as usize;
                let name_offset = reader.read_u32().ok_or(DtbError::Structure)? as usize;
                let name = property_name(strings, name_offset)?;
                let value = reader.read_bytes(length).ok_or(DtbError::Structure)?;
                skip_padding(&mut reader, length)?;
                let value = value.as_slice();
                match name {
                    "#address-cells" if node.kind == NodeKind::ReservedMemory => {
                        if node.address_declared {
                            return Err(DtbError::Reservation);
                        }
                        node.address_cells = cell_count(value)?;
                        node.address_declared = true;
                    }
                    "#size-cells" if node.kind == NodeKind::ReservedMemory => {
                        if node.size_declared {
                            return Err(DtbError::Reservation);
                        }
                        node.size_cells = cell_count(value)?;
                        node.size_declared = true;
                    }
                    "timebase-frequency" if node.kind == NodeKind::Cpus => {
                        let frequency = scalar(value).ok_or(DtbError::Timebase)?;
                        if frequency == 0 || timebase.replace(frequency).is_some() {
                            return Err(DtbError::Timebase);
                        }
                    }
                    "ranges" if node.kind == NodeKind::ReservedMemory => {
                        if node.ranges || !value.is_empty() {
                            return Err(DtbError::UnsupportedReservation);
                        }
                        node.ranges = true;
                    }
                    "reg" if node.kind == NodeKind::Reservation => {
                        if node.reg.is_some() {
                            return Err(DtbError::Reservation);
                        }
                        node.reg = Some(value);
                    }
                    "size" if node.kind == NodeKind::Reservation => {
                        if node.requested_size.replace(value).is_some() {
                            return Err(DtbError::Reservation);
                        }
                    }
                    "alignment" if node.kind == NodeKind::Reservation => {
                        if node.alignment.replace(value).is_some() {
                            return Err(DtbError::Reservation);
                        }
                    }
                    "alloc-ranges" if node.kind == NodeKind::Reservation => {
                        if node.allocation_ranges.replace(value).is_some() {
                            return Err(DtbError::Reservation);
                        }
                    }
                    "no-map" if node.kind == NodeKind::Reservation => {
                        if !value.is_empty() || node.no_map || node.reusable {
                            return Err(DtbError::Reservation);
                        }
                        node.no_map = true;
                    }
                    "reusable" if node.kind == NodeKind::Reservation => {
                        if !value.is_empty() || node.reusable || node.no_map {
                            return Err(DtbError::Reservation);
                        }
                        node.reusable = true;
                    }
                    "status" if node.kind == NodeKind::Reservation => {
                        if node.status_declared {
                            return Err(DtbError::Reservation);
                        }
                        let status = cstring(value)?;
                        node.disabled = status != "okay" && status != "ok";
                        node.status_declared = true;
                    }
                    _ => {}
                }
            }
            Token::EndNode => {
                depth = depth.checked_sub(1).ok_or(DtbError::Structure)?;
                let node = stack[depth];
                match node.kind {
                    NodeKind::ReservedMemory
                        if !node.ranges || !node.address_declared || !node.size_declared =>
                    {
                        return Err(DtbError::UnsupportedReservation);
                    }
                    NodeKind::Reservation if !node.disabled => {
                        let Some(reg) = node.reg else {
                            validate_dynamic_request(&node)?;
                            continue;
                        };
                        // DTSpec 3.5.2 gives a static reg precedence over size;
                        // no dynamic constraint is used to place this range.
                        let tuple = (node.address_cells + node.size_cells) * 4;
                        if reg.is_empty() || !reg.len().is_multiple_of(tuple) {
                            return Err(DtbError::Reservation);
                        }
                        for tuple in reg.chunks_exact(tuple) {
                            let (address, size) = tuple.split_at(node.address_cells * 4);
                            let size = cells(size);
                            // Firmware may leave empty static placeholders;
                            // they reserve no memory, even with no-map.
                            if size == 0 {
                                continue;
                            }
                            append_reservation(reservations, cells(address), size, node.no_map)?;
                        }
                    }
                    _ => {}
                }
            }
            Token::Nop => {}
            Token::End => {
                if !seen_root || depth != 0 || !reader.remain().is_empty() {
                    return Err(DtbError::Structure);
                }
                break;
            }
            Token::Data(_) => return Err(DtbError::Structure),
        }
    }
    timebase.ok_or(DtbError::Timebase)
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum NodeKind {
    #[default]
    Other,
    Cpus,
    ReservedMemory,
    Reservation,
}

#[derive(Clone, Copy)]
struct NodeState<'a> {
    kind: NodeKind,
    address_cells: usize,
    size_cells: usize,
    address_declared: bool,
    size_declared: bool,
    has_children: bool,
    ranges: bool,
    requested_size: Option<&'a [u8]>,
    alignment: Option<&'a [u8]>,
    allocation_ranges: Option<&'a [u8]>,
    no_map: bool,
    reusable: bool,
    disabled: bool,
    status_declared: bool,
    reg: Option<&'a [u8]>,
}
impl Default for NodeState<'_> {
    fn default() -> Self {
        Self {
            kind: NodeKind::Other,
            address_cells: 2,
            size_cells: 1,
            address_declared: false,
            size_declared: false,
            has_children: false,
            ranges: false,
            requested_size: None,
            alignment: None,
            allocation_ranges: None,
            no_map: false,
            reusable: false,
            disabled: false,
            status_declared: false,
            reg: None,
        }
    }
}
fn validate_dynamic_request(node: &NodeState<'_>) -> Result<(), DtbError> {
    nonzero_cells(
        node.requested_size.ok_or(DtbError::Reservation)?,
        node.size_cells,
    )?;
    if let Some(alignment) = node.alignment {
        // DTSpec Table 3.5 encodes alignment using the parent's size cells.
        nonzero_cells(alignment, node.size_cells)?;
    }
    if let Some(ranges) = node.allocation_ranges {
        let tuple = (node.address_cells + node.size_cells) * 4;
        if ranges.is_empty() || !ranges.len().is_multiple_of(tuple) {
            return Err(DtbError::Reservation);
        }
        for range in ranges.chunks_exact(tuple) {
            let (address, size) = range.split_at(node.address_cells * 4);
            let size = nonzero_cells(size, node.size_cells)?;
            if cells(address).checked_add(size).is_none() {
                return Err(DtbError::Reservation);
            }
        }
    }
    // DTSpec 3.5.4 leaves dynamic placement to the OS after ExitBootServices.
    // alloc-ranges are candidate bounds, not already occupied physical memory.
    Ok(())
}
fn nonzero_cells(value: &[u8], count: usize) -> Result<u64, DtbError> {
    if value.len() != count * 4 {
        return Err(DtbError::Reservation);
    }
    let value = cells(value);
    if value == 0 {
        return Err(DtbError::Reservation);
    }
    Ok(value)
}
fn block_range(
    offset: u32,
    length: u32,
    total: usize,
    alignment: usize,
) -> Result<Range<usize>, DtbError> {
    let start = offset as usize;
    let end = start
        .checked_add(length as usize)
        .ok_or(DtbError::BlockRange)?;
    if start < HEADER_BYTES
        || end > total
        || !start.is_multiple_of(alignment)
        || !end.is_multiple_of(alignment)
    {
        return Err(DtbError::BlockRange);
    }
    Ok(start..end)
}
fn overlap(first: &Range<usize>, second: &Range<usize>) -> bool {
    !first.is_empty() && !second.is_empty() && first.start < second.end && second.start < first.end
}
fn append_reservation(
    reservations: &mut Vec<MemoryReservation>,
    address: u64,
    size: u64,
    no_map: bool,
) -> Result<(), DtbError> {
    if size == 0 || address.checked_add(size).is_none() {
        return Err(DtbError::Reservation);
    }
    reservations
        .try_reserve(1)
        .map_err(|_| DtbError::OutOfResources)?;
    reservations.push(MemoryReservation {
        address,
        size,
        no_map,
    });
    Ok(())
}
fn read_name<'a>(reader: &mut Reader<'a>) -> Result<&'a str, DtbError> {
    let remaining = reader.remain();
    let value =
        CStr::from_bytes_until_nul(remaining.as_slice()).map_err(|_| DtbError::Structure)?;
    let length = value.to_bytes_with_nul().len();
    reader.read_bytes(length).ok_or(DtbError::Structure)?;
    skip_padding(reader, length)?;
    value.to_str().map_err(|_| DtbError::Structure)
}
fn skip_padding(reader: &mut Reader<'_>, length: usize) -> Result<(), DtbError> {
    let padding = (4 - length % 4) % 4;
    reader.read_bytes(padding).ok_or(DtbError::Structure)?;
    Ok(())
}
fn property_name(strings: &[u8], offset: usize) -> Result<&str, DtbError> {
    let bytes = strings.get(offset..).ok_or(DtbError::Structure)?;
    let name = CStr::from_bytes_until_nul(bytes)
        .map_err(|_| DtbError::Structure)?
        .to_str()
        .map_err(|_| DtbError::Structure)?;
    if name.is_empty() {
        return Err(DtbError::Structure);
    }
    Ok(name)
}
fn cstring(value: &[u8]) -> Result<&str, DtbError> {
    CStr::from_bytes_with_nul(value)
        .map_err(|_| DtbError::Structure)?
        .to_str()
        .map_err(|_| DtbError::Structure)
}
fn scalar(value: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(value.try_into().ok()?))
}
fn cell_count(value: &[u8]) -> Result<usize, DtbError> {
    scalar(value)
        .filter(|value| matches!(value, 1 | 2))
        .map(|value| value as usize)
        .ok_or(DtbError::UnsupportedReservation)
}
fn cells(value: &[u8]) -> u64 {
    value.as_chunks::<4>().0.iter().fold(0, |result, word| {
        (result << 32) | u64::from(u32::from_be_bytes(*word))
    })
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::*;

    struct Tree {
        structure: Vec<u8>,
        strings: Vec<u8>,
    }
    impl Tree {
        fn word(&mut self, word: u32) {
            self.structure.extend_from_slice(&word.to_be_bytes());
        }
        fn begin(&mut self, name: &str) {
            self.word(1);
            self.structure.extend_from_slice(name.as_bytes());
            self.structure.push(0);
            while !self.structure.len().is_multiple_of(4) {
                self.structure.push(0);
            }
        }
        fn property(&mut self, name: &str, value: &[u8]) {
            let offset = self.strings.len();
            self.strings.extend_from_slice(name.as_bytes());
            self.strings.push(0);
            self.word(3);
            self.word(value.len() as u32);
            self.word(offset as u32);
            self.structure.extend_from_slice(value);
            while !self.structure.len().is_multiple_of(4) {
                self.structure.push(0);
            }
        }
        fn finish(self) -> Vec<u8> {
            let structure = 72;
            let strings = structure + self.structure.len();
            let total = strings + self.strings.len();
            let mut bytes = vec![0; structure];
            for (index, word) in [
                fdt_raw::FDT_MAGIC,
                total as u32,
                structure as u32,
                strings as u32,
                40,
                17,
                16,
                0,
                self.strings.len() as u32,
                self.structure.len() as u32,
            ]
            .into_iter()
            .enumerate()
            {
                bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
            }
            bytes[40..48].copy_from_slice(&0x9000_0000u64.to_be_bytes());
            bytes[48..56].copy_from_slice(&0x1000u64.to_be_bytes());
            bytes.extend_from_slice(&self.structure);
            bytes.extend_from_slice(&self.strings);
            bytes
        }
    }
    fn tree(address_cells: u32, size_cells: u32, reservation: &str) -> Vec<u8> {
        tree_with_reservation_properties(address_cells, size_cells, reservation, &[])
    }
    fn tree_with_reservation_properties(
        address_cells: u32,
        size_cells: u32,
        reservation: &str,
        properties: &[(&str, &[u8])],
    ) -> Vec<u8> {
        let mut reg = Vec::new();
        if address_cells == 2 {
            reg.extend_from_slice(&0u32.to_be_bytes());
        }
        reg.extend_from_slice(&0x9100_0000u32.to_be_bytes());
        if size_cells == 2 {
            reg.extend_from_slice(&0u32.to_be_bytes());
        }
        reg.extend_from_slice(&0x2000u32.to_be_bytes());
        let mut all = vec![(reservation, reg.as_slice())];
        all.extend_from_slice(properties);
        reservation_tree(address_cells, size_cells, "buffer@91000000", &all)
    }
    fn reservation_tree(
        address_cells: u32,
        size_cells: u32,
        name: &str,
        properties: &[(&str, &[u8])],
    ) -> Vec<u8> {
        let mut tree = Tree {
            structure: vec![],
            strings: vec![],
        };
        tree.begin("");
        tree.property("#address-cells", &2u32.to_be_bytes());
        tree.property("#size-cells", &2u32.to_be_bytes());
        tree.begin("cpus");
        tree.property("timebase-frequency", &24_000_000u32.to_be_bytes());
        tree.word(2);
        tree.begin("reserved-memory");
        tree.property("#address-cells", &address_cells.to_be_bytes());
        tree.property("#size-cells", &size_cells.to_be_bytes());
        tree.property("ranges", &[]);
        tree.begin(name);
        for (name, value) in properties {
            tree.property(name, value);
        }
        tree.word(2);
        tree.word(2);
        tree.word(2);
        tree.word(9);
        tree.finish()
    }

    #[test]
    fn boot_tree_validation_rejects_malformed_input_before_resource_publication() {
        let valid = tree(2, 1, "reg");
        let parsed = ValidatedFdt::parse(&valid).unwrap();
        let descriptor = |start, pages, ty| MemoryDescriptor {
            phys_start: start,
            page_count: pages,
            ty,
            ..MemoryDescriptor::default()
        };
        let mut map = [
            descriptor(0x9100_1000, 1, MemoryType::BOOT_SERVICES_DATA),
            descriptor(0x9000_0000, 1, MemoryType::RESERVED),
            descriptor(0x9100_0000, 1, MemoryType::BOOT_SERVICES_DATA),
        ];
        assert_eq!(parsed.validate_uefi_reservations(map.iter()), Ok(()));
        let no_map_bytes = tree_with_reservation_properties(2, 1, "reg", &[("no-map", &[])]);
        let no_map = ValidatedFdt::parse(&no_map_bytes).unwrap();
        assert!(matches!(
            no_map.validate_uefi_reservations(map.iter()),
            Err(DtbError::UnprotectedReservation(MemoryReservation {
                no_map: true,
                ..
            }))
        ));
        map[0].ty = MemoryType::RESERVED;
        map[2].ty = MemoryType::RESERVED;
        assert_eq!(no_map.validate_uefi_reservations(map.iter()), Ok(()));
        // A gap, allocatable RAM or overflowing descriptor cannot protect the
        // entire reservation, even when its first byte is covered.
        map[0].phys_start += 0x1000;
        assert!(parsed.validate_uefi_reservations(map.iter()).is_err());
        map[0].phys_start -= 0x1000;
        for kind in [
            MemoryType::CONVENTIONAL,
            MemoryType::LOADER_DATA,
            MemoryType::BOOT_SERVICES_CODE,
        ] {
            map[0].ty = kind;
            assert!(parsed.validate_uefi_reservations(map.iter()).is_err());
        }
        map[0].ty = MemoryType::RESERVED;
        map[0].page_count = u64::MAX;
        assert!(parsed.validate_uefi_reservations(map.iter()).is_err());
        assert_eq!(parsed.total_size(), valid.len());
        assert_eq!(
            parsed.reservations,
            [
                MemoryReservation {
                    address: 0x9000_0000,
                    size: 0x1000,
                    no_map: false,
                },
                MemoryReservation {
                    address: 0x9100_0000,
                    size: 0x2000,
                    no_map: false,
                },
            ]
        );
        // Empty memreserve entries must not hide later nonempty reservations.
        let mut empty_reservation = valid.clone();
        empty_reservation.splice(
            40..40,
            u64::MAX.to_be_bytes().into_iter().chain(0u64.to_be_bytes()),
        );
        for offset in [4, 8, 12] {
            let value = u32::from_be_bytes(valid[offset..offset + 4].try_into().unwrap());
            empty_reservation[offset..offset + 4].copy_from_slice(&(value + 16).to_be_bytes());
        }
        assert_eq!(
            ValidatedFdt::parse(&empty_reservation)
                .unwrap()
                .reservations,
            parsed.reservations
        );
        let mut capacity = valid.clone();
        capacity.extend_from_slice(&[0xff; 64]);
        assert_eq!(ValidatedFdt::parse(&capacity).unwrap().as_bytes(), valid);
        let mut malformed = valid.clone();
        let header = fdt_raw::Header::from_bytes(&valid).unwrap();
        let end = header.off_dt_struct as usize + header.size_dt_struct as usize;
        malformed[end - 4..end].copy_from_slice(&0xdeadbeefu32.to_be_bytes());
        assert!(fdt_raw::Fdt::from_bytes(&malformed).is_ok());
        assert!(
            ValidatedFdt::parse(&malformed).is_err(),
            "header-only validation accepted an invalid end token"
        );
        for (offset, word) in [
            (4, (MAX_DTB_BYTES + 1) as u32),
            (8, u32::MAX),
            (12, header.off_dt_struct),
            (16, 41),
            (36, header.size_dt_struct - 4),
        ] {
            let mut malformed = valid.clone();
            malformed[offset..offset + 4].copy_from_slice(&word.to_be_bytes());
            assert!(
                ValidatedFdt::parse(&malformed).is_err(),
                "header field at {offset}"
            );
        }
        for size in [0, 39, valid.len() - 1] {
            assert!(ValidatedFdt::parse(&valid[..size]).is_err());
        }
        // Change a referenced property name offset without changing any block
        // size: a header-only parser cannot notice the unsafe string lookup.
        let mut malformed = valid.clone();
        let property_name_offset = header.off_dt_struct as usize + 8 + 8;
        malformed[property_name_offset..property_name_offset + 4]
            .copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(ValidatedFdt::parse(&malformed).is_err());

        for (address_cells, size_cells) in [(1, 1), (2, 2)] {
            let alternative = tree(address_cells, size_cells, "reg");
            assert_eq!(
                ValidatedFdt::parse(&alternative).unwrap().reservations,
                parsed.reservations
            );
        }
        // Empty static tuples reserve no bytes, including no-map placeholders.
        // Keep checking nonempty tuples in the same property against the map.
        let reg = [
            0u32,
            0x9200_0000,
            0,
            0,
            0x9100_0000,
            0x2000,
            0,
            0x9300_0000,
            0,
        ]
        .into_iter()
        .flat_map(u32::to_be_bytes)
        .collect::<Vec<_>>();
        let placeholders =
            reservation_tree(2, 1, "buffer@92000000", &[("reg", &reg), ("no-map", &[])]);
        let placeholders = ValidatedFdt::parse(&placeholders).unwrap();
        assert_eq!(placeholders.reservations, no_map.reservations);
        assert_eq!(
            placeholders.validate_uefi_reservations(
                [
                    descriptor(0x9000_0000, 1, MemoryType::RESERVED),
                    descriptor(0x9100_0000, 2, MemoryType::RESERVED),
                ]
                .iter()
            ),
            Ok(())
        );
        // A request for OS allocation has no physical reservation before EBS.
        // Use an arbitrary node name: this rule is not specific to Linux CMA.
        let requested_size = 0x2000_0000u64.to_be_bytes();
        let alignment = 0x20_0000u64.to_be_bytes();
        let allocation_ranges = [
            1u32.to_be_bytes(),
            0x4000_0000u32.to_be_bytes(),
            0u32.to_be_bytes(),
            0x2000_0000u32.to_be_bytes(),
            2u32.to_be_bytes(),
            0u32.to_be_bytes(),
            0u32.to_be_bytes(),
            0x4000_0000u32.to_be_bytes(),
        ]
        .concat();
        let dynamic_properties: &[(&str, &[u8])] = &[
            ("size", &requested_size),
            ("alignment", &alignment),
            ("alloc-ranges", &allocation_ranges),
            ("reusable", &[]),
        ];
        let dynamic = reservation_tree(2, 2, "dma-pool", dynamic_properties);
        let dynamic_tree = ValidatedFdt::parse(&dynamic)
            .expect("valid dynamic request must not require an EFI reservation");
        assert_eq!(dynamic_tree.reservations, &parsed.reservations[..1]);
        let without_alignment = [&dynamic_properties[..1], &dynamic_properties[2..]].concat();
        for properties in [&dynamic_properties[..1], &without_alignment] {
            let optional_constraints = reservation_tree(2, 2, "pool", properties);
            assert_eq!(
                ValidatedFdt::parse(&optional_constraints)
                    .unwrap()
                    .reservations,
                dynamic_tree.reservations
            );
        }
        for (address_cells, size_cells) in [(1, 1), (1, 2), (2, 1)] {
            let size = &requested_size[8 - size_cells as usize * 4..];
            let boundary = &alignment[8 - size_cells as usize * 4..];
            let range = [
                0x4000_0000u64.to_be_bytes()[8 - address_cells as usize * 4..].to_vec(),
                0x4000_0000u64.to_be_bytes()[8 - size_cells as usize * 4..].to_vec(),
            ]
            .concat();
            let alternate = reservation_tree(
                address_cells,
                size_cells,
                "pool",
                &[
                    ("size", size),
                    ("alignment", boundary),
                    ("alloc-ranges", &range),
                ],
            );
            assert_eq!(
                ValidatedFdt::parse(&alternate).unwrap().reservations,
                dynamic_tree.reservations
            );
        }
        let static_with_request = tree_with_reservation_properties(
            2,
            1,
            "reg",
            &[("size", &[]), ("alignment", &[]), ("alloc-ranges", &[])],
        );
        assert_eq!(
            ValidatedFdt::parse(&static_with_request)
                .unwrap()
                .reservations,
            parsed.reservations
        );
        let invalid_dynamic: &[(&str, &[(&str, &[u8])])] = &[
            ("missing size", &[]),
            ("short size", &[("size", &[1])]),
            ("zero size", &[("size", &[0; 8])]),
            ("short alignment", &[("alignment", &[1])]),
            ("zero alignment", &[("alignment", &[0; 8])]),
            ("empty ranges", &[("alloc-ranges", &[])]),
            ("short ranges", &[("alloc-ranges", &[1])]),
            ("zero range size", &[("alloc-ranges", &[0; 16])]),
            ("overflowing range", &[("alloc-ranges", &[0xff; 16])]),
            ("duplicate size", &[("size", &requested_size)]),
            (
                "duplicate alignment",
                &[("alignment", &alignment), ("alignment", &alignment)],
            ),
            (
                "duplicate ranges",
                &[
                    ("alloc-ranges", &allocation_ranges),
                    ("alloc-ranges", &allocation_ranges),
                ],
            ),
            ("bad reg cannot fall back to size", &[("reg", &[1])]),
        ];
        for (name, properties) in invalid_dynamic {
            let mut all = vec![("size", requested_size.as_slice())];
            if matches!(*name, "missing size" | "short size" | "zero size") {
                all.clear();
            }
            all.extend_from_slice(properties);
            assert!(
                ValidatedFdt::parse(&reservation_tree(2, 2, "pool", &all)).is_err(),
                "invalid dynamic request accepted: {name}"
            );
        }
        for (address_cells, size_cells) in [(3, 1), (1, 0)] {
            assert!(ValidatedFdt::parse(&tree(address_cells, size_cells, "reg")).is_err());
        }
        let invalid_attributes: &[(&str, &[(&str, &[u8])])] = &[
            ("nonempty no-map", &[("no-map", &[1])]),
            ("duplicate no-map", &[("no-map", &[]), ("no-map", &[])]),
            ("nonempty reusable", &[("reusable", &[1])]),
            (
                "duplicate reusable",
                &[("reusable", &[]), ("reusable", &[])],
            ),
            (
                "no-map with reusable",
                &[("no-map", &[]), ("reusable", &[])],
            ),
            (
                "reusable with no-map",
                &[("reusable", &[]), ("no-map", &[])],
            ),
        ];
        let accepted: Vec<_> = invalid_attributes
            .iter()
            .filter_map(|(name, properties)| {
                ValidatedFdt::parse(&tree_with_reservation_properties(2, 1, "reg", properties))
                    .is_ok()
                    .then_some(*name)
            })
            .collect();
        assert!(
            accepted.is_empty(),
            "invalid reservation attributes accepted: {accepted:?}"
        );
        let no_map = tree_with_reservation_properties(2, 1, "reg", &[("no-map", &[])]);
        let no_map_tree = ValidatedFdt::parse(&no_map).unwrap();
        assert_eq!(
            no_map_tree.reservations,
            [
                MemoryReservation {
                    address: 0x9000_0000,
                    size: 0x1000,
                    no_map: false,
                },
                MemoryReservation {
                    address: 0x9100_0000,
                    size: 0x2000,
                    no_map: true,
                },
            ]
        );
        let reusable = tree_with_reservation_properties(2, 1, "reg", &[("reusable", &[])]);
        assert_eq!(
            ValidatedFdt::parse(&reusable).unwrap().reservations,
            parsed.reservations
        );
        let mut unterminated = valid.clone();
        unterminated[56..72].fill(0xff);
        assert!(ValidatedFdt::parse(&unterminated).is_err());
        let mut overflowing = valid.clone();
        overflowing[40..48].copy_from_slice(&u64::MAX.to_be_bytes());
        assert!(ValidatedFdt::parse(&overflowing).is_err());
        let mut missing_timebase = valid.clone();
        let frequency = valid
            .windows(4)
            .position(|word| word == 24_000_000u32.to_be_bytes())
            .unwrap();
        missing_timebase[frequency..frequency + 4].fill(0);
        assert!(matches!(
            ValidatedFdt::parse(&missing_timebase),
            Err(DtbError::Timebase)
        ));
        let mut oversized_property = valid.clone();
        let length = header.off_dt_struct as usize + 8 + 4;
        oversized_property[length..length + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(ValidatedFdt::parse(&oversized_property).is_err());
        let mut deep = Tree {
            structure: vec![],
            strings: vec![],
        };
        deep.begin("");
        for _ in 0..MAX_DEPTH {
            deep.begin("level");
        }
        for _ in 0..=MAX_DEPTH {
            deep.word(2);
        }
        deep.word(9);
        assert!(matches!(
            ValidatedFdt::parse(&deep.finish()),
            Err(DtbError::Structure)
        ));
    }
}
