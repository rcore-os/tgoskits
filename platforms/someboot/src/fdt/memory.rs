use core::ops::Range;

use heapless::Vec;
use ranges_ext::VecOp;

use crate::{
    boot_payload::InitramfsRange,
    consts::PAGE_SIZE,
    fdt::fdt_base,
    mem::{
        MemoryDescriptor, MemoryMap, MemoryType, NoMapError, NoMapRegions, add_memory_descriptor,
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

fn initramfs_from_fdt(fdt: fdt_raw::Fdt<'_>) -> Option<Result<InitramfsRange, &'static str>> {
    let chosen = fdt.chosen()?;
    let start = chosen.find_property("linux,initrd-start");
    let end = chosen.find_property("linux,initrd-end");
    if start.is_none() && end.is_none() {
        return None;
    }
    Some((|| {
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
        let contained = fdt.memory().any(|memory| {
            memory.regions().any(|region| {
                normalize_region(region.address, region.size)
                    .is_some_and(|ram| ram.start <= reservation_start && reservation_end <= ram.end)
            })
        });
        if !contained {
            return Err("initramfs reservation is outside usable RAM");
        }
        Ok(InitramfsRange {
            start,
            end,
            reclaimable: false,
        })
    })())
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

    if let Some(range) = initramfs_from_fdt(fdt)
        && crate::boot_payload::initramfs_range().is_none()
    {
        let range = range.unwrap_or_else(|error| panic!("invalid host initramfs: {error}"));
        let reservation = MemoryDescriptor::new_aligned(
            range.start,
            range.end - range.start,
            MemoryType::Reserved,
            PAGE_SIZE,
        );
        let reclaimable = archive_reservation_was_free(crate::mem::memory_map(), &reservation);
        match add_memory_descriptor(reservation.clone()) {
            Ok(()) => {}
            Err(error) => {
                let already_reserved = crate::mem::memory_map().iter().any(|entry| {
                    entry.memory_type == MemoryType::Reserved
                        && entry.physical_start <= reservation.physical_start
                        && entry.physical_start.saturating_add(entry.size_in_bytes)
                            >= reservation
                                .physical_start
                                .saturating_add(reservation.size_in_bytes)
                });
                assert!(
                    already_reserved,
                    "failed to reserve host initramfs: {error:?}"
                );
            }
        }
        crate::boot_payload::publish(range.start, range.end, reclaimable);
    }

    Some(())
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

pub fn memories() -> impl Iterator<Item = Range<usize>> {
    #[cfg(efi)]
    let efi_map = crate::efi_stub::is_uefi_available().then(crate::mem::memory_map);
    #[cfg(not(efi))]
    let efi_map = None;
    ram_regions(fdt_base().as_ref(), efi_map)
        .expect("invalid boot RAM mapping")
        .into_iter()
}

fn ram_regions(
    fdt: Option<&fdt_raw::Fdt<'_>>,
    efi_map: Option<&[MemoryDescriptor]>,
) -> Result<Vec<Range<usize>, 128>, NoMapError> {
    let mut res = Vec::<Range<usize>, 128>::new();
    let excluded = fdt.map(no_map_regions).transpose()?.unwrap_or_default();
    let mut append = |region| -> Result<(), NoMapError> {
        for fragment in excluded.subtract(region)? {
            // EFI ownership descriptors can split a contiguous RAM bank many
            // times. These mappings all use the same permissions.
            if let Some(previous) = res.last_mut()
                && previous.end == fragment.start
            {
                previous.end = fragment.end;
                continue;
            }
            res.push(fragment).map_err(|_| NoMapError::Capacity)?;
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
                append(checked_region(region.address, region.size)?)?;
            }
        }
    }
    Ok(res)
}

struct StaticReservation {
    range: Range<usize>,
    no_map: bool,
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
        checked_region(address, size)?;
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

fn static_reservations(fdt: &fdt_raw::Fdt<'_>) -> Result<Vec<StaticReservation, 128>, NoMapError> {
    let mut result = Vec::new();
    let Some(parent) = fdt.find_by_path("/reserved-memory") else {
        return Ok(result);
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
            let range = checked_region(value(address), value(size))?;
            let range = crate::mem::no_map::page_range(range)?;
            result
                .push(StaticReservation {
                    range,
                    no_map: no_map.is_some(),
                })
                .map_err(|_| NoMapError::Capacity)?;
        }
    }
    Ok(result)
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
    for reservation in static_reservations(fdt)? {
        if reservation.no_map {
            exclusions.insert(reservation.range)?;
        }
    }
    Ok(exclusions)
}

pub(crate) fn reserve_static_memory(
    map: &mut MemoryMap,
    fdt: &fdt_raw::Fdt<'_>,
) -> Result<(), NoMapError> {
    let insert = |map: &mut MemoryMap, range| {
        map.merge_add(MemoryDescriptor::new_with_range(
            range,
            MemoryType::Reserved,
        ))
        .map_err(|error| match error {
            ranges_ext::RangeError::Capacity => NoMapError::Capacity,
            _ => NoMapError::ReservationConflict,
        })
    };
    for entry in fdt.memory_reservations() {
        insert(
            map,
            crate::mem::no_map::page_range(checked_region(entry.address, entry.size)?)?,
        )?;
    }
    for reservation in static_reservations(fdt)? {
        insert(map, reservation.range)?;
    }
    Ok(())
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

    #[test]
    fn static_reservations_protect_every_tuple_before_ram_mapping() {
        use fdt_edit::{Fdt, Node, Property};

        let mut tree = Fdt::new();
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
        let reg = [0x1000u64.to_be_bytes(), 0x10000u64.to_be_bytes()].concat();
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
        let reg = [0x6001u64, 1, 0x2001, 0x1000, 0x3001, 1]
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
        let fdt = fdt_raw::Fdt::from_bytes(bytes.as_ref()).unwrap();
        let excluded = no_map_regions(&fdt).unwrap();
        let mut map = MemoryMap::new();
        map.push(MemoryDescriptor::new_with_range(
            0x1000..0x11000,
            MemoryType::Free,
        ))
        .unwrap();
        reserve_static_memory(&mut map, &fdt).unwrap();
        for page in [0x2000, 0x3000, 0x6000] {
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
                .as_slice(),
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
        let bounded = NoMapRegions::from_fdt(tree.encode().as_ref()).unwrap();
        assert!(matches!(
            bounded.subtract(0..0x300000),
            Err(NoMapError::Capacity)
        ));
        tree.node_mut(reserved)
            .unwrap()
            .set_property(Property::new("reg", many));
        assert!(matches!(
            NoMapRegions::from_fdt(tree.encode().as_ref()),
            Err(NoMapError::Capacity)
        ));
        for reg in [alloc::vec![0; 16], alloc::vec![0xff; 16], alloc::vec![1]] {
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
