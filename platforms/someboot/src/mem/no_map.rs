//! Page exclusions derived from the current DTB, without persistent registration.

use core::ops::Range;

use heapless::Vec;

use super::{MemoryDescriptor, MemoryType};
use crate::consts::PAGE_SIZE;

const MAX_REGIONS: usize = 128;

/// A malformed or unrepresentable boot memory exclusion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NoMapError {
    #[error("invalid FDT reservation encoding")]
    InvalidFdt,
    #[error("invalid or overflowing physical memory range")]
    InvalidRange,
    #[error("boot memory range capacity exceeded")]
    Capacity,
    #[error("required mapping overlaps FDT no-map pages: {start:#x}..{end:#x}")]
    MappingConflict { start: usize, end: usize },
    #[error("failed to reserve FDT physical memory")]
    ReservationConflict,
}

/// Sorted, merged page exclusions derived from the current FDT.
///
/// Values are short lived snapshots, never an additional reservation registry.
/// At most 128 disjoint regions and 128 output fragments are supported.
#[derive(Debug, Default)]
pub struct NoMapRegions {
    ranges: Vec<Range<usize>, MAX_REGIONS>,
}

impl NoMapRegions {
    /// Derives exclusions from all static no-map reg tuples in the current DTB.
    /// An absent DTB has no exclusions; malformed or excessive input fails.
    /// Firmware may place its input DTB in an excluded region; `save_fdt`
    /// validates the RAM copy before boot page tables are constructed.
    pub fn current() -> Result<Self, NoMapError> {
        if crate::fdt::fdt_addr_phys().is_none() {
            return Ok(Self::default());
        }
        let bytes = crate::fdt::fdt_bytes().ok_or(NoMapError::InvalidFdt)?;
        Self::from_fdt(bytes)
    }

    /// Checks bounded binary framing and static reservation encodings, then
    /// derives no-map exclusions without validating every device binding.
    /// No firmware state or physical allocation is modified.
    pub fn from_fdt(bytes: &[u8]) -> Result<Self, NoMapError> {
        crate::fdt::no_map_regions(&crate::fdt::checked_memory_tree(bytes)?)
    }

    pub(crate) fn insert(&mut self, range: Range<usize>) -> Result<(), NoMapError> {
        let mut range = page_range(range)?;
        let mut index = 0;
        while index < self.ranges.len() {
            let existing = &self.ranges[index];
            if existing.end < range.start || range.end < existing.start {
                index += 1;
            } else {
                range.start = range.start.min(existing.start);
                range.end = range.end.max(existing.end);
                self.ranges.remove(index);
            }
        }
        self.ranges.push(range).map_err(|_| NoMapError::Capacity)?;
        self.ranges.sort_unstable_by_key(|range| range.start);
        Ok(())
    }

    /// Returns all candidate RAM fragments outside excluded pages.
    pub fn subtract(
        &self,
        candidate: Range<usize>,
    ) -> Result<Vec<Range<usize>, MAX_REGIONS>, NoMapError> {
        if candidate.start >= candidate.end {
            return Err(NoMapError::InvalidRange);
        }
        let mut fragments = Vec::new();
        let mut cursor = candidate.start;
        for excluded in &self.ranges {
            if excluded.end <= cursor {
                continue;
            }
            if excluded.start >= candidate.end {
                break;
            }
            if cursor < excluded.start {
                fragments
                    .push(cursor..excluded.start)
                    .map_err(|_| NoMapError::Capacity)?;
            }
            cursor = cursor.max(excluded.end);
            if cursor >= candidate.end {
                return Ok(fragments);
            }
        }
        if cursor < candidate.end {
            fragments
                .push(cursor..candidate.end)
                .map_err(|_| NoMapError::Capacity)?;
        }
        Ok(fragments)
    }

    /// Rejects any required mapping that would expose an excluded byte.
    pub fn ensure_mappable(&self, range: Range<usize>) -> Result<(), NoMapError> {
        if range.start >= range.end {
            return Err(NoMapError::InvalidRange);
        }
        if self
            .ranges
            .iter()
            .any(|excluded| range.start < excluded.end && excluded.start < range.end)
        {
            return Err(NoMapError::MappingConflict {
                start: range.start,
                end: range.end,
            });
        }
        Ok(())
    }
}

pub(crate) fn page_range(range: Range<usize>) -> Result<Range<usize>, NoMapError> {
    if range.start >= range.end {
        return Err(NoMapError::InvalidRange);
    }
    let end = range
        .end
        .checked_next_multiple_of(PAGE_SIZE)
        .ok_or(NoMapError::InvalidRange)?;
    Ok((range.start & !(PAGE_SIZE - 1))..end)
}

/// Constructs the complete ordinary reserved-RAM mapping list.
/// Required live resources fail on exclusions instead of being clipped.
pub fn reserved_ram_regions(
    map: &[MemoryDescriptor],
    excluded: &NoMapRegions,
) -> Result<Vec<(usize, usize), MAX_REGIONS>, NoMapError> {
    let mut list = Vec::new();
    for region in map {
        let start = region.physical_start;
        let end = start
            .checked_add(region.size_in_bytes)
            .ok_or(NoMapError::InvalidRange)?;
        match region.memory_type {
            MemoryType::Reserved => {
                for fragment in excluded.subtract(start..end)? {
                    list.push((fragment.start, fragment.len()))
                        .map_err(|_| NoMapError::Capacity)?;
                }
            }
            MemoryType::KImage
            | MemoryType::PerCpuData
            | MemoryType::BootData
            | MemoryType::BootReclaimable => {
                excluded.ensure_mappable(start..end)?;
                list.push((start, end - start))
                    .map_err(|_| NoMapError::Capacity)?;
            }
            _ => {}
        }
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    #[test]
    fn reserved_ram_mapping_excludes_no_map_and_rejects_required_backing() {
        let word =
            |structure: &mut Vec<u8>, value: u32| structure.extend_from_slice(&value.to_be_bytes());
        let mut structure = Vec::new();
        word(&mut structure, 1);
        word(&mut structure, 0);
        word(&mut structure, 1);
        structure.extend_from_slice(b"reserved-memory\0");
        for (offset, value) in [
            (0, &2u32.to_be_bytes()[..]),
            (15, &2u32.to_be_bytes()[..]),
            (27, &[][..]),
        ] {
            word(&mut structure, 3);
            word(&mut structure, value.len() as u32);
            word(&mut structure, offset);
            structure.extend_from_slice(value);
        }
        word(&mut structure, 1);
        structure.extend_from_slice(b"pool\0\0\0\0");
        word(&mut structure, 3);
        word(&mut structure, 16);
        word(&mut structure, 34);
        structure.extend_from_slice(&0x2001u64.to_be_bytes());
        structure.extend_from_slice(&0x1000u64.to_be_bytes());
        for value in [3, 0, 38, 2, 2, 2, 9] {
            word(&mut structure, value);
        }
        let strings = b"#address-cells\0#size-cells\0ranges\0reg\0no-map\0";
        let mut bytes = alloc::vec![0u8; 56];
        let total = 56 + structure.len() + strings.len();
        for (index, value) in [
            0xd00dfeed,
            total as u32,
            56,
            (56 + structure.len()) as u32,
            40,
            17,
            16,
            0,
            strings.len() as u32,
            structure.len() as u32,
        ]
        .into_iter()
        .enumerate()
        {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
        }
        bytes.extend_from_slice(&structure);
        bytes.extend_from_slice(strings);
        let excluded = NoMapRegions::from_fdt(&bytes).unwrap();
        let map = [MemoryDescriptor::new_with_range(
            0x1000..0x9000,
            MemoryType::Reserved,
        )];
        assert_eq!(
            reserved_ram_regions(&map, &excluded).unwrap().as_slice(),
            &[(0x1000, 0x1000), (0x4000, 0x5000)]
        );
        for kind in [
            MemoryType::KImage,
            MemoryType::PerCpuData,
            MemoryType::BootData,
            MemoryType::BootReclaimable,
        ] {
            let required = [MemoryDescriptor::new_with_range(0x3000..0x5000, kind)];
            assert!(matches!(
                reserved_ram_regions(&required, &excluded),
                Err(NoMapError::MappingConflict { .. })
            ));
        }
        let empty = NoMapRegions::default();
        assert_eq!(
            reserved_ram_regions(&map, &empty).unwrap().as_slice(),
            &[(0x1000, 0x8000)]
        );
    }
}
