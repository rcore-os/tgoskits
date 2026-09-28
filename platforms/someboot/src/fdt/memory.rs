use core::ops::Range;

use heapless::Vec;

use crate::{
    boot_payload::InitramfsRange,
    consts::PAGE_SIZE,
    fdt::fdt_base,
    mem::{MemoryDescriptor, MemoryType, add_memory_descriptor},
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

    for reserved in fdt.memory_reservations() {
        let Some(region) = normalize_region(reserved.address, reserved.size) else {
            continue;
        };
        add_memory_descriptor(MemoryDescriptor::new_aligned(
            region.start,
            region.end - region.start,
            MemoryType::Reserved,
            PAGE_SIZE,
        ))
        .unwrap();
    }

    for reserved in fdt.reserved_memory() {
        if let Some(mut itr) = reserved.reg()
            && let Some(reg) = itr.next()
            && let Some(size) = reg.size
            && let Some(region) = normalize_region(reg.address, size)
        {
            add_memory_descriptor(MemoryDescriptor {
                physical_start: region.start,
                size_in_bytes: region.end - region.start,
                memory_type: MemoryType::Reserved,
            })
            .unwrap();
        }
    }

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
    let mut res = Vec::<_, 128>::new();
    if let Some(fdt) = fdt_base() {
        for memory in fdt.memory() {
            for region in memory.regions() {
                if let Some(region) = normalize_region(region.address, region.size) {
                    res.push(region).ok();
                }
            }
        }
    }
    res.into_iter()
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
