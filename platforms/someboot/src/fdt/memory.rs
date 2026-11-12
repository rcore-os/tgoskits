use core::ops::Range;

use heapless::Vec;
use ranges_ext::RangeOp;

use crate::{
    boot_payload::InitramfsRange,
    consts::PAGE_SIZE,
    mem::{
        MEMORY_MAP_CAPACITY, MemoryDescriptor, MemoryMap, MemoryType, NoMapError, NoMapRegions,
        add_memory_descriptor,
    },
};

fn decode_address(bytes: &[u8]) -> Option<usize> {
    let value = match bytes.len() {
        4 => u32::from_be_bytes(bytes.try_into().ok()?) as u64,
        8 => u64::from_be_bytes(bytes.try_into().ok()?),
        _ => return None,
    };
    usize::try_from(value).ok()
}

pub(crate) fn initramfs_from_fdt(
    fdt: fdt_raw::Fdt<'_>,
    covers_ram: impl FnOnce(&Range<usize>) -> bool,
) -> Result<Option<InitramfsRange>, NoMapError> {
    let Some(chosen) = fdt.chosen() else {
        return Ok(None);
    };
    let start = chosen.find_property("linux,initrd-start");
    let end = chosen.find_property("linux,initrd-end");
    if start.is_none() && end.is_none() {
        return Ok(None);
    }
    (|| {
        let start = decode_address(start.ok_or("missing linux,initrd-start")?.as_slice())
            .ok_or("invalid linux,initrd-start")?;
        let end = decode_address(end.ok_or("missing linux,initrd-end")?.as_slice())
            .ok_or("invalid linux,initrd-end")?;
        let start = normalize_fdt_address(start);
        let end = normalize_fdt_address(end);
        if start >= end {
            return Err("empty or reversed initramfs range");
        }
        no_map_regions(&fdt)
            .map_err(|_| "invalid FDT no-map reservations")?
            .ensure_mappable(start..end)
            .map_err(|_| "initramfs overlaps FDT no-map pages")?;
        let reservation_start = start & !(PAGE_SIZE - 1);
        let reservation_end = end
            .checked_next_multiple_of(PAGE_SIZE)
            .ok_or("initramfs reservation alignment overflows")?;
        if !covers_ram(&(reservation_start..reservation_end)) {
            return Err("initramfs reservation is outside usable RAM");
        }
        Ok(Some(InitramfsRange {
            start,
            end,
            reclaimable: false,
        }))
    })()
    .map_err(|_: &'static str| NoMapError::InvalidFdt)
}

pub(crate) fn direct_boot_initramfs(
    fdt: &fdt_raw::Fdt<'_>,
) -> Result<Option<InitramfsRange>, NoMapError> {
    initramfs_from_fdt(fdt.clone(), |requested| {
        fdt.memory().any(|memory| {
            memory.regions().any(|region| {
                normalize_region(region.address, region.size)
                    .is_some_and(|ram| ram.start <= requested.start && requested.end <= ram.end)
            })
        })
    })
}

pub fn init_memory_map() -> Option<()> {
    let fdt = super::fdt_base()?;

    #[cfg(efi)]
    let add_fdt_ram = !crate::efi_stub::is_uefi_available();
    #[cfg(not(efi))]
    let add_fdt_ram = true;
    if add_fdt_ram {
        for memory in fdt.memory() {
            for region in memory.regions() {
                let Some(region) = normalize_region(region.address, region.size) else {
                    continue;
                };

                add_memory_descriptor(MemoryDescriptor {
                    physical_start: region.start,
                    size_in_bytes: region.end - region.start,
                    memory_type: MemoryType::Free,
                })
                .unwrap();
            }
        }
    }

    crate::mem::reserve_fdt_memory(&fdt).expect("failed to reserve FDT memory before allocation");

    Some(())
}

pub(crate) fn reserve_fdt_initramfs(
    map: &mut MemoryMap,
    mut archive: InitramfsRange,
) -> Result<InitramfsRange, NoMapError> {
    let reservation = MemoryDescriptor::new_aligned(
        archive.start,
        archive.end - archive.start,
        MemoryType::Reserved,
        PAGE_SIZE,
    );
    archive.reclaimable = archive_reservation_was_free(map, &reservation);
    let requested = reservation.range();
    if let Err(error) = crate::mem::insert_memory_descriptor(map, reservation) {
        let already_reserved = map.iter().any(|entry| {
            let current = entry.range();
            entry.memory_type == MemoryType::Reserved
                && current.start <= requested.start
                && current.end >= requested.end
        });
        if !already_reserved {
            return Err(match error {
                ranges_ext::RangeError::Capacity => NoMapError::Capacity,
                ranges_ext::RangeError::Conflict { .. } => NoMapError::ReservationConflict,
            });
        }
    }
    Ok(archive)
}

fn archive_reservation_was_free(
    memory_map: &[MemoryDescriptor],
    reservation: &MemoryDescriptor,
) -> bool {
    let start = reservation.physical_start;
    let Some(end) = start.checked_add(reservation.size_in_bytes) else {
        return false;
    };
    memory_map.iter().any(|entry| {
        entry.memory_type == MemoryType::Free
            && entry.physical_start <= start
            && entry
                .physical_start
                .checked_add(entry.size_in_bytes)
                .is_some_and(|free_end| end <= free_end)
    })
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) fn for_each_memory(
    visit: impl FnMut(Range<usize>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    #[cfg(efi)]
    let efi_map = crate::efi_stub::is_uefi_available().then(crate::mem::memory_map);
    #[cfg(not(efi))]
    let efi_map = None;
    for_each_ram_region(super::fdt_base().as_ref(), efi_map, visit)
}

#[cfg(any(test, target_arch = "aarch64", target_arch = "riscv64"))]
fn for_each_ram_region<E: From<NoMapError>>(
    fdt: Option<&fdt_raw::Fdt<'_>>,
    efi_map: Option<&[MemoryDescriptor]>,
    visit: impl FnMut(Range<usize>) -> Result<(), E>,
) -> Result<(), E> {
    let excluded = fdt.map(no_map_regions).transpose()?.unwrap_or_default();
    for_each_mapped_region(fdt, efi_map, &excluded, visit)
}

#[cfg(all(efi, target_arch = "riscv64"))]
pub(crate) fn for_each_efi_ram_region(
    map: &[MemoryDescriptor],
    excluded: &NoMapRegions,
    visit: impl FnMut(Range<usize>) -> Result<(), NoMapError>,
) -> Result<(), NoMapError> {
    for_each_mapped_region(None, Some(map), excluded, visit)
}

#[cfg(any(test, target_arch = "aarch64", target_arch = "riscv64"))]
fn for_each_mapped_region<E: From<NoMapError>>(
    fdt: Option<&fdt_raw::Fdt<'_>>,
    efi_map: Option<&[MemoryDescriptor]>,
    excluded: &NoMapRegions,
    mut visit: impl FnMut(Range<usize>) -> Result<(), E>,
) -> Result<(), E> {
    let mut pending: Option<Range<usize>> = None;
    let mut append = |region| -> Result<(), E> {
        for fragment in excluded.subtract(region)? {
            // EFI ownership descriptors can split a contiguous RAM bank many
            // times. These mappings all use the same permissions.
            if let Some(previous) = pending.as_mut()
                && previous.end == fragment.start
            {
                previous.end = fragment.end;
                continue;
            }
            if let Some(previous) = pending.replace(fragment) {
                visit(previous)?;
            }
        }
        Ok(())
    };
    if let Some(map) = efi_map {
        // DTSpec 3.4.1 makes the EFI map authoritative for RAM. The shared
        // normalized map already retains kernel, boot and firmware storage;
        // keep those mappings while excluding MMIO and all FDT no-map pages.
        for memory in map
            .iter()
            .filter(|entry| entry.memory_type != MemoryType::Mmio)
        {
            append(checked_region(
                memory.physical_start as u64,
                memory.size_in_bytes as u64,
            )?)?;
        }
    } else if let Some(fdt) = fdt {
        for memory in fdt.memory() {
            for region in memory.regions() {
                // U-Boot can include unused RAM banks as empty reg tuples.
                if region.size == 0 {
                    continue;
                }
                append(checked_region(region.address, region.size)?)?;
            }
        }
    }
    if let Some(last) = pending {
        visit(last)?;
    }
    Ok(())
}

/// Gates every raw token/name/block read without constructing a second tree.
/// Boot input is bounded to 16 MiB and 16 nested node levels.
pub(crate) fn checked_memory_tree(bytes: &[u8]) -> Result<fdt_raw::Fdt<'_>, NoMapError> {
    use core::ffi::CStr;

    use fdt_raw::{Token, data::Bytes};

    let invalid = NoMapError::InvalidFdt;
    let header = fdt_raw::Header::from_bytes(bytes).map_err(|_| invalid)?;
    let total = header.totalsize as usize;
    if !(40..=16 * 1024 * 1024).contains(&total)
        || header.version < 17
        || header.last_comp_version > 17
        || header.last_comp_version > header.version
    {
        return Err(invalid);
    }
    let bytes = bytes.get(..total).ok_or(invalid)?;
    let block = |start: u32, size: u32, alignment: usize| {
        let start = start as usize;
        let end = start.checked_add(size as usize).ok_or(invalid)?;
        if start < 40
            || end > total
            || !start.is_multiple_of(alignment)
            || !end.is_multiple_of(alignment)
        {
            return Err(invalid);
        }
        Ok(start..end)
    };
    let structure = block(header.off_dt_struct, header.size_dt_struct, 4)?;
    let strings = block(header.off_dt_strings, header.size_dt_strings, 1)?;
    let overlaps = |a: &Range<usize>, b: &Range<usize>| {
        !a.is_empty() && !b.is_empty() && a.start < b.end && b.start < a.end
    };
    if structure.is_empty() || overlaps(&structure, &strings) {
        return Err(invalid);
    }
    let reserve_start = header.off_mem_rsvmap as usize;
    if reserve_start < 40 || !reserve_start.is_multiple_of(8) {
        return Err(invalid);
    }
    let mut reserve = Bytes::new(bytes.get(reserve_start..).ok_or(invalid)?).reader();
    loop {
        let address = reserve.read_u64().ok_or(invalid)?;
        let size = reserve.read_u64().ok_or(invalid)?;
        if address == 0 && size == 0 {
            break;
        }
        if size != 0 {
            checked_region(address, size)?;
        }
    }
    let reserve = reserve_start..reserve_start + reserve.position();
    if overlaps(&reserve, &structure) || overlaps(&reserve, &strings) {
        return Err(invalid);
    }
    let strings = &bytes[strings];
    let mut reader = Bytes::new(&bytes[structure]).reader();
    let mut depth = 0usize;
    let mut seen_root = false;
    let mut seen_reserved_memory = false;
    let mut children = [false; 16];
    loop {
        match reader.read_token().map_err(|_| invalid)? {
            Token::BeginNode => {
                if depth == children.len() || depth == 0 && seen_root {
                    return Err(invalid);
                }
                let remaining = reader.remain();
                let name = CStr::from_bytes_until_nul(remaining.as_slice()).map_err(|_| invalid)?;
                let text = name.to_str().map_err(|_| invalid)?;
                if text == "reserved-memory"
                    && (depth != 1 || core::mem::replace(&mut seen_reserved_memory, true))
                {
                    return Err(invalid);
                }
                if depth == 0 && !text.is_empty()
                    || depth != 0 && (text.is_empty() || text.contains('/'))
                {
                    return Err(invalid);
                }
                let length = name.to_bytes_with_nul().len();
                reader.read_bytes(length).ok_or(invalid)?;
                reader.read_bytes((4 - length % 4) % 4).ok_or(invalid)?;
                if depth > 0 {
                    children[depth - 1] = true;
                }
                children[depth] = false;
                depth += 1;
                seen_root = true;
            }
            Token::Prop => {
                if depth == 0 || children[depth - 1] {
                    return Err(invalid);
                }
                let length = reader.read_u32().ok_or(invalid)? as usize;
                let offset = reader.read_u32().ok_or(invalid)? as usize;
                let name = CStr::from_bytes_until_nul(strings.get(offset..).ok_or(invalid)?)
                    .map_err(|_| invalid)?;
                if name.to_bytes().is_empty() || name.to_str().is_err() {
                    return Err(invalid);
                }
                reader.read_bytes(length).ok_or(invalid)?;
                reader.read_bytes((4 - length % 4) % 4).ok_or(invalid)?;
            }
            Token::EndNode => depth = depth.checked_sub(1).ok_or(invalid)?,
            Token::Nop => {}
            Token::End if seen_root && depth == 0 && reader.remain().is_empty() => break,
            _ => return Err(invalid),
        }
    }
    fdt_raw::Fdt::from_bytes(bytes).map_err(|_| invalid)
}

fn checked_region(address: u64, size: u64) -> Result<Range<usize>, NoMapError> {
    address.checked_add(size).ok_or(NoMapError::InvalidRange)?;
    let start =
        normalize_fdt_address(usize::try_from(address).map_err(|_| NoMapError::InvalidRange)?);
    let size = usize::try_from(size).map_err(|_| NoMapError::InvalidRange)?;
    if size == 0 {
        return Err(NoMapError::InvalidRange);
    }
    Ok(start..start.checked_add(size).ok_or(NoMapError::InvalidRange)?)
}

fn for_each_static_reservation(
    fdt: &fdt_raw::Fdt<'_>,
    mut visit: impl FnMut(Range<usize>, bool) -> Result<(), NoMapError>,
) -> Result<(), NoMapError> {
    let Some(parent) = fdt.find_by_path("/reserved-memory") else {
        return Ok(());
    };
    let count = |name| {
        unique_property(&parent, name)?
            .and_then(|property| {
                let raw: [u8; 4] = property.as_slice().try_into().ok()?;
                Some(u32::from_be_bytes(raw) as usize)
            })
            .filter(|count| matches!(count, 1 | 2))
            .ok_or(NoMapError::InvalidFdt)
    };
    let address_cells = count("#address-cells")?;
    let size_cells = count("#size-cells")?;
    if !unique_property(&parent, "ranges")?.is_some_and(|ranges| ranges.as_slice().is_empty()) {
        return Err(NoMapError::InvalidFdt);
    }
    for node in fdt.reserved_memory() {
        if node.level() != parent.level() + 1 {
            return Err(NoMapError::InvalidFdt);
        }
        if let Some(status) = unique_property(&node, "status")? {
            let status = core::ffi::CStr::from_bytes_with_nul(status.as_slice())
                .map_err(|_| NoMapError::InvalidFdt)?
                .to_str()
                .map_err(|_| NoMapError::InvalidFdt)?;
            if status != "ok" && status != "okay" {
                continue;
            }
        }
        let no_map = unique_property(&node, "no-map")?;
        if no_map.is_some() && unique_property(&node, "reusable")?.is_some() {
            return Err(NoMapError::InvalidFdt);
        }
        if no_map
            .as_ref()
            .is_some_and(|property| !property.as_slice().is_empty())
        {
            return Err(NoMapError::InvalidFdt);
        }
        let Some(reg) = unique_property(&node, "reg")? else {
            if unique_property(&node, "size")?.is_some() {
                continue;
            }
            return Err(NoMapError::InvalidFdt);
        };
        let tuple_bytes = (address_cells + size_cells) * 4;
        let bytes = reg.as_slice();
        if bytes.is_empty() || !bytes.len().is_multiple_of(tuple_bytes) {
            return Err(NoMapError::InvalidFdt);
        }
        let value = |bytes: &[u8]| {
            bytes.as_chunks::<4>().0.iter().fold(0u64, |value, cell| {
                (value << 32) | u64::from(u32::from_be_bytes(*cell))
            })
        };
        for tuple in bytes.chunks_exact(tuple_bytes) {
            let (address, size) = tuple.split_at(address_cells * 4);
            let size = value(size);
            // Firmware may leave empty static reservation placeholders.
            if size == 0 {
                continue;
            }
            let range = checked_region(value(address), size)?;
            visit(crate::mem::no_map::page_range(range)?, no_map.is_some())?;
        }
    }
    Ok(())
}

fn unique_property<'a>(
    node: &fdt_raw::Node<'a>,
    name: &str,
) -> Result<Option<fdt_raw::Property<'a>>, NoMapError> {
    let mut properties = node.properties().filter(|property| property.name() == name);
    let first = properties.next();
    if properties.next().is_some() {
        return Err(NoMapError::InvalidFdt);
    }
    Ok(first)
}

pub(crate) fn no_map_regions(fdt: &fdt_raw::Fdt<'_>) -> Result<NoMapRegions, NoMapError> {
    let mut exclusions = NoMapRegions::default();
    for_each_merged_reservation(fdt, false, |range| exclusions.insert(range))?;
    Ok(exclusions)
}

fn for_each_merged_reservation(
    fdt: &fdt_raw::Fdt<'_>,
    include_ordinary: bool,
    mut visit: impl FnMut(Range<usize>) -> Result<(), NoMapError>,
) -> Result<(), NoMapError> {
    let mut scratch = NoMapRegions::default();
    let mut last: Option<Range<usize>> = None;
    let mut pending = Vec::<_, { usize::BITS as usize }>::new();
    pending
        .push(0..(usize::MAX & !(PAGE_SIZE - 1)))
        .map_err(|_| NoMapError::Capacity)?;
    while let Some(window) = pending.pop() {
        scratch.clear();
        let mut start = window.end;
        let mut end = window.start;
        let mut partition = false;
        let mut collect = |range: Range<usize>| {
            let clipped = range.start.max(window.start)..range.end.min(window.end);
            if !clipped.is_empty() {
                start = start.min(clipped.start);
                end = end.max(clipped.end);
                if !partition {
                    match scratch.insert(clipped) {
                        Ok(()) => {}
                        Err(NoMapError::Capacity) => partition = true,
                        Err(error) => return Err(error),
                    }
                }
            }
            Ok(())
        };
        if include_ordinary {
            for entry in fdt.memory_reservations().filter(|entry| entry.size != 0) {
                let range = checked_region(entry.address, entry.size)?;
                collect(crate::mem::no_map::page_range(range)?)?;
            }
        }
        for_each_static_reservation(fdt, |range, no_map| {
            if include_ordinary || no_map {
                collect(range)?;
            }
            Ok(())
        })?;
        if partition {
            // A full prefix can still be joined by later input. Split the
            // occupied address span, then merge complete windows left to right.
            let middle = (start + (end - start) / 2) & !(PAGE_SIZE - 1);
            pending
                .push(middle..end)
                .map_err(|_| NoMapError::Capacity)?;
            pending
                .push(start..middle)
                .map_err(|_| NoMapError::Capacity)?;
        } else {
            for range in scratch.ranges() {
                if let Some(previous) = &mut last
                    && previous.end == range.start
                {
                    previous.end = range.end;
                } else if let Some(previous) = last.replace(range.clone()) {
                    visit(previous)?;
                }
            }
        }
    }
    if let Some(range) = last {
        visit(range)?;
    }
    Ok(())
}

pub(crate) fn reserve_static_memory(
    map: &mut MemoryMap,
    fdt: &fdt_raw::Fdt<'_>,
) -> Result<NoMapRegions, NoMapError> {
    // Check the independent exclusion limit even when ordinary reservations
    // would join the no-map ranges in the memory map.
    let exclusions = no_map_regions(fdt)?;

    let mut ordered: Vec<_, MEMORY_MAP_CAPACITY> = map.iter().collect();
    ordered.sort_unstable_by_key(|entry| entry.physical_start);
    let mut remaining = ordered.iter().copied().cloned();
    let mut next = remaining.next();
    let mut result = MemoryMap::new();
    // Both streams are address ordered. Emit final fragments without inserting
    // partially applied reservations into the fixed-capacity memory map.
    for_each_merged_reservation(fdt, true, |range| {
        let reservation = MemoryDescriptor::new_with_range(range.clone(), MemoryType::Reserved);
        while let Some(existing) = next.take() {
            let current = existing.range();
            if current.start >= range.end {
                next = Some(existing);
                break;
            }
            if current.end <= range.start {
                append_memory_descriptor(&mut result, existing)?;
            } else {
                if !(existing.overwritable(&reservation) || existing.mergeable(&reservation)) {
                    return Err(NoMapError::ReservationConflict);
                }
                if current.start < range.start {
                    let left = existing.clone_with_range(current.start..range.start);
                    append_memory_descriptor(&mut result, left)?;
                }
                if current.end > range.end {
                    next = Some(existing.clone_with_range(range.end..current.end));
                    break;
                }
            }
            next = remaining.next();
        }
        append_memory_descriptor(&mut result, reservation)
    })?;
    for existing in next.into_iter().chain(remaining) {
        append_memory_descriptor(&mut result, existing)?;
    }
    drop(ordered);
    *map = result;
    Ok(exclusions)
}

fn append_memory_descriptor(
    map: &mut MemoryMap,
    entry: MemoryDescriptor,
) -> Result<(), NoMapError> {
    if let Some(previous) = map.last_mut()
        && previous.mergeable(&entry)
        && previous.range().end == entry.physical_start
    {
        previous.size_in_bytes += entry.size_in_bytes;
        return Ok(());
    }
    map.push(entry).map_err(|_| NoMapError::Capacity)
}

fn normalize_region(address: u64, size: u64) -> Option<Range<usize>> {
    if size == 0 {
        return None;
    }

    let start = normalize_fdt_address(address as usize);
    let size = size as usize;
    let end = start.checked_add(size)?;
    Some(start..end)
}

fn normalize_fdt_address(address: usize) -> usize {
    <crate::arch::Arch as crate::ArchTrait>::canonicalize_paddr(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ram_regions(
        fdt: Option<&fdt_raw::Fdt<'_>>,
        efi_map: Option<&[MemoryDescriptor]>,
    ) -> Result<alloc::vec::Vec<Range<usize>>, NoMapError> {
        let mut regions = alloc::vec::Vec::new();
        for_each_ram_region(fdt, efi_map, |range| {
            regions.push(range);
            Ok(())
        })?;
        Ok(regions)
    }

    #[test]
    fn static_reservations_protect_every_tuple_before_ram_mapping() {
        use fdt_edit::{Fdt, MemoryReservation, Node, Property};

        let mut tree = Fdt::new();
        tree.memory_reservations.extend([
            MemoryReservation {
                address: u64::MAX,
                size: 0,
            },
            MemoryReservation {
                address: 0x8001,
                size: 1,
            },
        ]);
        let root = tree.root_id();
        for name in ["#address-cells", "#size-cells"] {
            tree.node_mut(root)
                .unwrap()
                .set_property(Property::new(name, 2u32.to_be_bytes().to_vec()));
        }
        let memory = tree.add_node(root, Node::new("memory@1000"));
        tree.node_mut(memory)
            .unwrap()
            .set_property(Property::new("device_type", b"memory\0".to_vec()));
        let reg = [0u64, 0, 0x1000, 0x10000, u64::MAX, 0]
            .into_iter()
            .flat_map(u64::to_be_bytes)
            .collect();
        tree.node_mut(memory)
            .unwrap()
            .set_property(Property::new("reg", reg));
        let parent = tree.add_node(root, Node::new("reserved-memory"));
        for name in ["#address-cells", "#size-cells"] {
            tree.node_mut(parent)
                .unwrap()
                .set_property(Property::new(name, 2u32.to_be_bytes().to_vec()));
        }
        tree.node_mut(parent)
            .unwrap()
            .set_property(Property::new("ranges", alloc::vec![]));
        let reserved = tree.add_node(parent, Node::new("device-buffer"));
        let reg = [0u64, 0, 0x6001, 1, 0x2001, 0x1000, 0x3001, 1, 0x9000, 0]
            .into_iter()
            .flat_map(u64::to_be_bytes)
            .collect();
        tree.node_mut(reserved)
            .unwrap()
            .set_property(Property::new("reg", reg));
        tree.node_mut(reserved)
            .unwrap()
            .set_property(Property::new("no-map", alloc::vec![]));
        let bytes = tree.encode();
        NoMapRegions::from_fdt(bytes.as_ref()).unwrap();
        let fdt = fdt_raw::Fdt::from_bytes(bytes.as_ref()).unwrap();
        let excluded = no_map_regions(&fdt).unwrap();
        let mut map = MemoryMap::new();
        map.push(MemoryDescriptor::new_with_range(
            0x1000..0x11000,
            MemoryType::Free,
        ))
        .unwrap();
        reserve_static_memory(&mut map, &fdt).unwrap();
        for page in [0x2000, 0x3000, 0x6000, 0x8000] {
            assert!(
                map.iter()
                    .any(|region| region.memory_type == MemoryType::Reserved
                        && region.physical_start <= page
                        && page + PAGE_SIZE <= region.physical_start + region.size_in_bytes)
            );
            assert!(
                !map.iter()
                    .any(|region| region.memory_type == MemoryType::Free
                        && region.physical_start <= page
                        && page < region.physical_start + region.size_in_bytes)
            );
        }
        assert_eq!(
            ram_regions(Some(&fdt), None).unwrap().as_slice(),
            &[0x1000..0x2000, 0x4000..0x6000, 0x7000..0x11000]
        );
        // EFI owns the RAM inventory even when the DTB omits /memory or
        // describes a different range. FDT no-map still constrains mapping.
        let efi_map = [
            MemoryDescriptor::new_with_range(0x5000..0x9000, MemoryType::Ram),
            MemoryDescriptor::new_with_range(0x9000..0xb000, MemoryType::Free),
            MemoryDescriptor::new_with_range(0xb000..0xc000, MemoryType::Mmio),
            MemoryDescriptor::new_with_range(0xc000..0xe000, MemoryType::Reserved),
        ];
        assert_eq!(
            ram_regions(Some(&fdt), Some(&efi_map)).unwrap().as_slice(),
            &[0x5000..0x6000, 0x7000..0xb000, 0xc000..0xe000]
        );
        assert_eq!(
            ram_regions(None, Some(&efi_map)).unwrap().as_slice(),
            &[0x5000..0xb000, 0xc000..0xe000]
        );
        tree.remove_node(root, "memory@1000").unwrap();
        let without_memory_bytes = tree.encode();
        let without_memory = fdt_raw::Fdt::from_bytes(without_memory_bytes.as_ref()).unwrap();
        assert_eq!(
            ram_regions(Some(&without_memory), Some(&efi_map))
                .unwrap()
                .as_slice(),
            &[0x5000..0x6000, 0x7000..0xb000, 0xc000..0xe000]
        );
        assert!(ram_regions(Some(&without_memory), None).unwrap().is_empty());
        assert!(ram_regions(Some(&fdt), Some(&[])).unwrap().is_empty());
        assert!(excluded.ensure_mappable(0x4000..0x6000).is_ok());
        assert!(matches!(
            excluded.ensure_mappable(0x1fff..0x2001),
            Err(NoMapError::MappingConflict { .. })
        ));
        // This image's byte range is clear, but its actual 2 MiB map tail is not.
        assert!(crate::mem::required_boot_mappings(&excluded, 0x1000..0x1800, 0..0).is_err());
        assert!(
            crate::mem::required_boot_mappings(&excluded, 0x400000..0x401000, 0x6000..0x7000)
                .is_err()
        );
        assert!(
            crate::mem::required_boot_mappings(&excluded, 0x400000..0x401000, 0x7000..0x8000)
                .is_ok()
        );
        assert_eq!(
            NoMapRegions::default()
                .subtract(0x1000..0x11000)
                .unwrap()
                .collect::<alloc::vec::Vec<_>>(),
            &[0x1000..0x11000]
        );
        for (offset, value) in [(8, u32::MAX), (16, 41), (12, 40)] {
            let mut malformed = bytes.as_ref().to_vec();
            malformed[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
            assert!(NoMapRegions::from_fdt(&malformed).is_err());
        }
        assert!(NoMapRegions::from_fdt(&bytes.as_ref()[..39]).is_err());
        let mut many = alloc::vec::Vec::new();
        for index in 0..129u64 {
            many.extend_from_slice(&(0x100000 + index * 0x2000).to_be_bytes());
            many.extend_from_slice(&1u64.to_be_bytes());
        }
        tree.node_mut(reserved)
            .unwrap()
            .set_property(Property::new("reg", many[..128 * 16].to_vec()));
        let fragmented_bytes = tree.encode();
        let fragmented = fdt_raw::Fdt::from_bytes(fragmented_bytes.as_ref()).unwrap();
        let efi_map = [MemoryDescriptor::new_with_range(
            0..0x300000,
            MemoryType::Free,
        )];
        let fragment_count =
            ram_regions(Some(&fragmented), Some(&efi_map)).map(|ranges| ranges.len());
        tree.node_mut(reserved)
            .unwrap()
            .set_property(Property::new("reg", many));
        assert!(matches!(
            NoMapRegions::from_fdt(tree.encode().as_ref()),
            Err(NoMapError::Capacity)
        ));
        tree.memory_reservations.clear();
        let start = 0x100000usize;
        let end = start + 257 * PAGE_SIZE;
        // Every other page appears before the intervening pages, so early
        // prefixes can have more runs than the final contiguous reservation.
        let adjacent = (start..end)
            .step_by(2 * PAGE_SIZE)
            .chain((start + PAGE_SIZE..end).step_by(2 * PAGE_SIZE))
            .flat_map(|address| [address as u64, PAGE_SIZE as u64])
            .flat_map(u64::to_be_bytes)
            .collect();
        tree.node_mut(reserved)
            .unwrap()
            .set_property(Property::new("reg", adjacent));
        let adjacent_bytes = tree.encode();
        let merged_count = NoMapRegions::from_fdt(adjacent_bytes.as_ref()).and_then(|excluded| {
            excluded
                .subtract(start - PAGE_SIZE..end + PAGE_SIZE)
                .map(Iterator::count)
        });
        assert_eq!((fragment_count, merged_count), (Ok(129), Ok(2)));
        let excluded = NoMapRegions::from_fdt(adjacent_bytes.as_ref()).unwrap();
        assert_eq!(
            excluded
                .subtract(start - PAGE_SIZE..end + PAGE_SIZE)
                .unwrap()
                .collect::<alloc::vec::Vec<_>>(),
            &[start - PAGE_SIZE..start, end..end + PAGE_SIZE]
        );
        let fdt = fdt_raw::Fdt::from_bytes(adjacent_bytes.as_ref()).unwrap();
        let mut map = MemoryMap::new();
        map.push(MemoryDescriptor::new_with_range(
            start..end,
            MemoryType::Free,
        ))
        .unwrap();
        reserve_static_memory(&mut map, &fdt).unwrap();
        assert_eq!(
            map.as_slice(),
            &[MemoryDescriptor::new_with_range(
                start..end,
                MemoryType::Reserved
            )]
        );

        // The reservation block and ordinary reg tuples form a single run,
        // even though the reservation block alone exceeds the boot map.
        let end = start + (2 * map.capacity() + 1) * PAGE_SIZE;
        tree.memory_reservations
            .extend(
                (start..end)
                    .step_by(2 * PAGE_SIZE)
                    .map(|address| MemoryReservation {
                        address: address as u64,
                        size: PAGE_SIZE as u64,
                    }),
            );
        let ordinary = (start + PAGE_SIZE..end)
            .step_by(2 * PAGE_SIZE)
            .flat_map(|address| [address as u64, PAGE_SIZE as u64])
            .flat_map(u64::to_be_bytes)
            .collect();
        let node = tree.node_mut(reserved).unwrap();
        node.remove_property("no-map");
        node.set_property(Property::new("reg", ordinary));
        let bytes = tree.encode();
        let fdt = fdt_raw::Fdt::from_bytes(bytes.as_ref()).unwrap();
        let mut map = MemoryMap::new();
        map.push(MemoryDescriptor::new_with_range(
            start..end + PAGE_SIZE,
            MemoryType::Free,
        ))
        .unwrap();
        reserve_static_memory(&mut map, &fdt).unwrap();
        assert!(
            NoMapRegions::from_fdt(bytes.as_ref())
                .unwrap()
                .ranges()
                .is_empty()
        );
        assert_eq!(
            map.iter()
                .find(|region| region.memory_type == MemoryType::Reserved),
            Some(&MemoryDescriptor::new_with_range(
                start..end,
                MemoryType::Reserved
            ))
        );
        assert_eq!(
            map.iter()
                .find(|region| region.memory_type == MemoryType::Free),
            Some(&MemoryDescriptor::new_with_range(
                end..end + PAGE_SIZE,
                MemoryType::Free
            ))
        );
        assert_eq!(map.len(), 2);

        // Without the intervening pages these reservations really do exceed
        // capacity. A failed overlay must leave the original map intact.
        let node = tree.node_mut(reserved).unwrap();
        node.set_property(Property::new("reg", [0; 16].to_vec()));
        let bytes = tree.encode();
        let fdt = fdt_raw::Fdt::from_bytes(bytes.as_ref()).unwrap();
        let mut map = MemoryMap::new();
        map.push(MemoryDescriptor::new_with_range(
            start..end,
            MemoryType::Free,
        ))
        .unwrap();
        let before = map.clone();
        assert_eq!(
            reserve_static_memory(&mut map, &fdt).unwrap_err(),
            NoMapError::Capacity
        );
        assert_eq!(map, before);

        tree.node_mut(reserved)
            .unwrap()
            .set_property(Property::new("no-map", alloc::vec![]));
        for reg in [alloc::vec![0xff; 16], alloc::vec![1]] {
            tree.node_mut(reserved)
                .unwrap()
                .set_property(Property::new("reg", reg));
            assert!(NoMapRegions::from_fdt(tree.encode().as_ref()).is_err());
        }
    }

    #[test]
    fn preexisting_reservation_is_not_owned_by_the_archive() {
        let archive =
            MemoryDescriptor::new_aligned(0x2000, 0x1000, MemoryType::Reserved, PAGE_SIZE);
        let free = MemoryDescriptor {
            physical_start: 0x1000,
            size_in_bytes: 0x3000,
            memory_type: MemoryType::Free,
        };
        let reserved = MemoryDescriptor {
            memory_type: MemoryType::Reserved,
            ..free.clone()
        };
        assert!(archive_reservation_was_free(&[free], &archive));
        assert!(!archive_reservation_was_free(&[reserved], &archive));
        assert!(!archive_reservation_was_free(
            &[
                MemoryDescriptor {
                    physical_start: 0x1000,
                    size_in_bytes: 0x1000,
                    memory_type: MemoryType::Free,
                },
                archive.clone(),
                MemoryDescriptor {
                    physical_start: 0x3000,
                    size_in_bytes: 0x1000,
                    memory_type: MemoryType::Free,
                },
            ],
            &archive
        ));
    }
}
