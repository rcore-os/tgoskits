//! Reads the fixed UEFI memory-map buffer used at ExitBootServices.

use core::mem::size_of;

use uefi::{Status, boot::MemoryDescriptor};

pub(crate) const EXIT_BOOT_MEMORY_MAP_BUFFER_SIZE: usize = 128 * 1024;

#[repr(align(8))]
pub(crate) struct AlignedBytes<const N: usize>(pub(crate) [u8; N]);

type GetMemoryMap = unsafe extern "efiapi" fn(
    *mut usize,
    *mut MemoryDescriptor,
    *mut usize,
    *mut usize,
    *mut u32,
) -> Status;

pub(crate) struct RawBootMemoryMap<'a> {
    bytes: &'a [u8],
    pub(crate) key: usize,
    pub(crate) desc_size: usize,
    pub(crate) desc_version: u32,
}

impl RawBootMemoryMap<'_> {
    pub(crate) fn entries(&self) -> impl ExactSizeIterator<Item = MemoryDescriptor> + Clone + '_ {
        self.bytes.chunks_exact(self.desc_size).map(|entry| {
            // SAFETY: the reader validated the stride. The raw UEFI descriptor
            // is plain data, and the descriptor prefix is fully initialized.
            unsafe { entry.as_ptr().cast::<MemoryDescriptor>().read_unaligned() }
        })
    }
}

#[cfg(any(target_arch = "riscv64", test))]
pub(crate) fn covers_initramfs_ram(
    descriptors: impl Iterator<Item = MemoryDescriptor> + Clone,
    range: &core::ops::Range<usize>,
) -> bool {
    use uefi::boot::MemoryType;

    let mut cursor = range.start as u64;
    while cursor < range.end as u64 {
        let next = descriptors
            .clone()
            .filter(|entry| {
                matches!(
                    entry.ty,
                    MemoryType::LOADER_DATA
                        | MemoryType::BOOT_SERVICES_DATA
                        | MemoryType::CONVENTIONAL
                        | MemoryType::ACPI_RECLAIM
                ) && entry.phys_start <= cursor
            })
            .filter_map(|entry| {
                entry
                    .page_count
                    .checked_mul(4096)?
                    .checked_add(entry.phys_start)
            })
            .filter(|end| *end > cursor)
            .max();
        let Some(next) = next else {
            return false;
        };
        cursor = next;
    }
    true
}

/// # Safety
/// The firmware callback must follow the GetMemoryMap buffer contract.
pub(crate) unsafe fn read_exit_boot_memory_map(
    get_memory_map: GetMemoryMap,
    buffer: &mut AlignedBytes<EXIT_BOOT_MEMORY_MAP_BUFFER_SIZE>,
) -> core::result::Result<RawBootMemoryMap<'_>, Status> {
    let mut map_size = buffer.0.len();
    let mut key = 0;
    let mut desc_size = 0;
    let mut desc_version = 0;
    // SAFETY: the aligned buffer uniquely owns map_size writable bytes.
    // Firmware receives valid outputs and does not retain any pointer.
    let status = unsafe {
        get_memory_map(
            &mut map_size,
            buffer.0.as_mut_ptr().cast(),
            &mut key,
            &mut desc_size,
            &mut desc_version,
        )
    };
    match status {
        Status::BUFFER_TOO_SMALL => return Err(Status::OUT_OF_RESOURCES),
        Status::SUCCESS => {}
        error => return Err(error),
    }
    let bytes = buffer.0.get(..map_size).ok_or(Status::OUT_OF_RESOURCES)?;
    if bytes.is_empty()
        || desc_size < size_of::<MemoryDescriptor>()
        || !map_size.is_multiple_of(desc_size)
    {
        return Err(Status::COMPROMISED_DATA);
    }
    Ok(RawBootMemoryMap {
        bytes,
        key,
        desc_size,
        desc_version,
    })
}

#[cfg(test)]
mod tests {
    use uefi::boot::MemoryType;

    use super::*;

    #[test]
    fn raw_boot_map_uses_the_exit_buffer_limit() {
        const STRIDE: usize = 64;
        const CAPACITY: usize = EXIT_BOOT_MEMORY_MAP_BUFFER_SIZE / STRIDE;

        unsafe extern "efiapi" fn get_map<const COUNT: usize>(
            size: *mut usize,
            map: *mut MemoryDescriptor,
            key: *mut usize,
            desc_size: *mut usize,
            desc_version: *mut u32,
        ) -> Status {
            let needed = COUNT * STRIDE;
            // SAFETY: the test calls this through the EFI reader, which passes
            // valid output pointers and an aligned map buffer of the given size.
            unsafe {
                let available = size.replace(needed);
                if available < needed {
                    return Status::BUFFER_TOO_SMALL;
                }
                map.write(MemoryDescriptor {
                    ty: MemoryType::LOADER_DATA,
                    phys_start: 0x1000,
                    page_count: 1,
                    ..MemoryDescriptor::default()
                });
                key.write(17);
                desc_size.write(STRIDE);
                desc_version.write(1);
            }
            Status::SUCCESS
        }

        let mut buffer = AlignedBytes([0; EXIT_BOOT_MEMORY_MAP_BUFFER_SIZE]);
        // SAFETY: get_map implements the GetMemoryMap contract for this buffer.
        let map = unsafe { read_exit_boot_memory_map(get_map::<CAPACITY>, &mut buffer) }.unwrap();
        assert_eq!(map.key, 17);
        assert_eq!(map.desc_version, 1);
        assert_eq!(map.entries().len(), CAPACITY);
        assert_eq!(map.entries().next().unwrap().phys_start, 0x1000);

        // SAFETY: the oversized mock reports the required size without writing.
        let oversized =
            unsafe { read_exit_boot_memory_map(get_map::<{ CAPACITY + 1 }>, &mut buffer) };
        assert!(matches!(oversized, Err(Status::OUT_OF_RESOURCES)));
    }
}
