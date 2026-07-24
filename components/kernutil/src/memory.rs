use core::fmt::{Debug, Display};

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct MemoryDescriptor {
    pub physical_start: usize,
    pub size_in_bytes: usize,
    pub memory_type: MemoryType,
}

impl MemoryDescriptor {
    pub fn new_with_range(range: core::ops::Range<usize>, memory_type: MemoryType) -> Self {
        MemoryDescriptor {
            physical_start: range.start,
            size_in_bytes: range.end - range.start,
            memory_type,
        }
    }

    pub fn new_with_range_aligned(
        range: core::ops::Range<usize>,
        memory_type: MemoryType,
        align: usize,
    ) -> Result<Self, MemoryRangeError> {
        if range.start > range.end {
            return Err(MemoryRangeError::InvalidRange);
        }
        let align_mask = align
            .checked_sub(1)
            .filter(|_| align.is_power_of_two())
            .ok_or(MemoryRangeError::InvalidAlignment)?;
        let aligned_end = range
            .end
            .checked_add(align_mask)
            .ok_or(MemoryRangeError::InvalidRange)?
            & !align_mask;
        Ok(Self::new_with_range(
            (range.start & !align_mask)..aligned_end,
            memory_type,
        ))
    }

    pub fn new_aligned(
        physical_start: usize,
        size_in_bytes: usize,
        memory_type: MemoryType,
        align: usize,
    ) -> Result<Self, MemoryRangeError> {
        let end = physical_start
            .checked_add(size_in_bytes)
            .ok_or(MemoryRangeError::InvalidRange)?;
        Self::new_with_range_aligned(physical_start..end, memory_type, align)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemoryType {
    #[default]
    Free,
    Ram,
    KImage,
    Reserved,
    Mmio,
    PerCpuData,
}

impl Display for MemoryType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            MemoryType::Free => "Free  ",
            MemoryType::Ram => "RAM   ",
            MemoryType::KImage => "KImg  ",
            MemoryType::Reserved => "Rsv   ",
            MemoryType::Mmio => "MMIO  ",
            MemoryType::PerCpuData => "PerCPU",
        };
        write!(f, "{}", s)
    }
}

/// Error returned while updating a fixed-capacity boot memory map.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MemoryRangeError {
    /// The descriptor range overflows the address type.
    #[error("memory descriptor range overflows")]
    InvalidRange,
    /// An alignment is zero or is not a power of two.
    #[error("memory descriptor alignment is invalid")]
    InvalidAlignment,
    /// The fixed-capacity map cannot hold the resulting descriptors.
    #[error("memory map capacity exceeded")]
    Capacity,
    /// A descriptor overlaps memory that cannot be replaced.
    #[error("memory descriptor {new:?} conflicts with {existing:?}")]
    Conflict {
        /// Descriptor being inserted.
        new: MemoryDescriptor,
        /// Existing non-overwritable descriptor.
        existing: MemoryDescriptor,
    },
}

/// Fixed-capacity operations required by the boot memory map.
pub trait MemoryMapExt {
    /// Inserts a descriptor, splitting free ranges and merging adjacent ranges
    /// of the same type. The map is unchanged on error.
    fn merge_add(&mut self, descriptor: MemoryDescriptor) -> Result<(), MemoryRangeError>;
}

impl<const N: usize> MemoryMapExt for heapless::Vec<MemoryDescriptor, N> {
    fn merge_add(&mut self, descriptor: MemoryDescriptor) -> Result<(), MemoryRangeError> {
        let new_range = descriptor_range(&descriptor)?;
        let mut planned = self.clone();
        let mut index = 0;

        while index < planned.len() {
            let existing = planned[index].clone();
            let existing_range = descriptor_range(&existing)?;
            if new_range.start >= existing_range.end || new_range.end <= existing_range.start {
                index += 1;
                continue;
            }
            if existing.memory_type != MemoryType::Free
                && existing.memory_type != descriptor.memory_type
            {
                return Err(MemoryRangeError::Conflict {
                    new: descriptor,
                    existing,
                });
            }

            planned.remove(index);
            if existing_range.start < new_range.start {
                planned
                    .insert(
                        index,
                        MemoryDescriptor::new_with_range(
                            existing_range.start..new_range.start,
                            existing.memory_type,
                        ),
                    )
                    .map_err(|_| MemoryRangeError::Capacity)?;
                index += 1;
            }
            if new_range.end < existing_range.end {
                planned
                    .insert(
                        index,
                        MemoryDescriptor::new_with_range(
                            new_range.end..existing_range.end,
                            existing.memory_type,
                        ),
                    )
                    .map_err(|_| MemoryRangeError::Capacity)?;
                index += 1;
            }
        }

        planned
            .push(descriptor)
            .map_err(|_| MemoryRangeError::Capacity)?;
        merge_same_type(&mut planned)?;
        *self = planned;
        Ok(())
    }
}

fn descriptor_range(
    descriptor: &MemoryDescriptor,
) -> Result<core::ops::Range<usize>, MemoryRangeError> {
    let end = descriptor
        .physical_start
        .checked_add(descriptor.size_in_bytes)
        .ok_or(MemoryRangeError::InvalidRange)?;
    Ok(descriptor.physical_start..end)
}

fn merge_same_type<const N: usize>(
    descriptors: &mut heapless::Vec<MemoryDescriptor, N>,
) -> Result<(), MemoryRangeError> {
    loop {
        let mut pair = None;
        for left in 0..descriptors.len() {
            let left_range = descriptor_range(&descriptors[left])?;
            for right in left + 1..descriptors.len() {
                let right_range = descriptor_range(&descriptors[right])?;
                if descriptors[left].memory_type == descriptors[right].memory_type
                    && left_range.end >= right_range.start
                    && right_range.end >= left_range.start
                {
                    pair = Some((left, right, left_range, right_range));
                    break;
                }
            }
            if pair.is_some() {
                break;
            }
        }

        let Some((left, right, left_range, right_range)) = pair else {
            return Ok(());
        };
        let memory_type = descriptors[left].memory_type;
        descriptors.remove(right);
        descriptors.remove(left);
        descriptors
            .insert(
                left,
                MemoryDescriptor::new_with_range(
                    left_range.start.min(right_range.start)..left_range.end.max(right_range.end),
                    memory_type,
                ),
            )
            .map_err(|_| MemoryRangeError::Capacity)?;
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PageTableInfo {
    pub asid: usize,
    pub addr: usize,
}

impl PageTableInfo {
    pub const fn zero() -> Self {
        PageTableInfo { asid: 0, addr: 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_memory_map_splits_free_range_and_merges_equal_neighbors() {
        let mut map = heapless::Vec::<MemoryDescriptor, 4>::new();
        map.push(MemoryDescriptor::new_with_range(
            0..0x4000,
            MemoryType::Free,
        ))
        .unwrap();

        map.merge_add(MemoryDescriptor::new_with_range(
            0x1000..0x2000,
            MemoryType::Reserved,
        ))
        .unwrap();
        map.merge_add(MemoryDescriptor::new_with_range(
            0x2000..0x3000,
            MemoryType::Reserved,
        ))
        .unwrap();

        assert!(map.contains(&MemoryDescriptor::new_with_range(
            0x1000..0x3000,
            MemoryType::Reserved,
        )));
    }

    #[test]
    fn boot_memory_map_capacity_failure_preserves_original_map() {
        let original = MemoryDescriptor::new_with_range(0..0x4000, MemoryType::Free);
        let mut map = heapless::Vec::<MemoryDescriptor, 1>::new();
        map.push(original.clone()).unwrap();

        assert_eq!(
            map.merge_add(MemoryDescriptor::new_with_range(
                0x1000..0x2000,
                MemoryType::Reserved,
            )),
            Err(MemoryRangeError::Capacity),
        );
        assert_eq!(map.as_slice(), &[original]);
    }

    #[test]
    fn boot_memory_map_conflict_preserves_original_map() {
        let original = MemoryDescriptor::new_with_range(0..0x2000, MemoryType::KImage);
        let mut map = heapless::Vec::<MemoryDescriptor, 2>::new();
        map.push(original.clone()).unwrap();

        assert!(matches!(
            map.merge_add(MemoryDescriptor::new_with_range(
                0x1000..0x3000,
                MemoryType::Reserved,
            )),
            Err(MemoryRangeError::Conflict { .. })
        ));
        assert_eq!(map.as_slice(), &[original]);
    }

    #[test]
    fn aligned_descriptor_rejects_address_overflow() {
        assert_eq!(
            MemoryDescriptor::new_aligned(usize::MAX - 0xfff, 0x2000, MemoryType::Reserved, 0x1000,),
            Err(MemoryRangeError::InvalidRange),
        );
    }
}
