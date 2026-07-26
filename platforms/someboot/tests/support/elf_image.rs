use std::ptr;

const PAGE_SIZE: usize = 4096;

#[derive(Clone, Copy)]
struct LoadSegment {
    file_offset: usize,
    virtual_address: u64,
    file_size: usize,
    memory_size: usize,
}

#[derive(Clone, Copy)]
pub enum MappingPermissions {
    ReadWrite,
    ReadWriteExecute,
}

impl MappingPermissions {
    fn protection(self) -> libc::c_int {
        match self {
            Self::ReadWrite => libc::PROT_READ | libc::PROT_WRITE,
            Self::ReadWriteExecute => libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
        }
    }
}

pub struct ElfImage {
    link_base: usize,
    entry_offset: usize,
    mapping_size: usize,
    segments: Vec<LoadSegment>,
}

impl ElfImage {
    pub fn parse(image: &[u8]) -> Self {
        assert_eq!(&image[..4], b"\x7fELF", "fixture must be an ELF image");
        assert_eq!(image[4], 2, "fixture must use ELF64");
        let entry = read_u64(image, 24);
        let program_offset = usize::try_from(read_u64(image, 32)).unwrap();
        let program_size = usize::from(read_u16(image, 54));
        let program_count = usize::from(read_u16(image, 56));
        assert_eq!(program_size, 56, "fixture must use native ELF64 phdrs");

        let mut segments = Vec::new();
        for index in 0..program_count {
            let offset = program_offset + index * program_size;
            if read_u32(image, offset) != 1 {
                continue;
            }
            let segment = LoadSegment {
                file_offset: usize::try_from(read_u64(image, offset + 8)).unwrap(),
                virtual_address: read_u64(image, offset + 16),
                file_size: usize::try_from(read_u64(image, offset + 32)).unwrap(),
                memory_size: usize::try_from(read_u64(image, offset + 40)).unwrap(),
            };
            assert!(segment.file_size <= segment.memory_size);
            assert!(segment.file_offset + segment.file_size <= image.len());
            segments.push(segment);
        }
        assert!(!segments.is_empty(), "fixture must contain PT_LOAD");
        let link_base = segments
            .iter()
            .map(|segment| segment.virtual_address as usize & !(PAGE_SIZE - 1))
            .min()
            .unwrap();
        let link_end = segments
            .iter()
            .map(|segment| usize::try_from(segment.virtual_address).unwrap() + segment.memory_size)
            .max()
            .unwrap();
        let mapping_size = align_up(link_end.wrapping_sub(link_base), PAGE_SIZE);
        let entry_offset = usize::try_from(entry).unwrap().wrapping_sub(link_base);
        assert!(entry_offset < mapping_size, "ELF entry must lie in PT_LOAD");
        Self {
            link_base,
            entry_offset,
            mapping_size,
            segments,
        }
    }

    pub fn load(&self, image: &[u8], permissions: MappingPermissions) -> LoadedElf {
        // SAFETY: arguments request a new private anonymous mapping. The return
        // value is checked against MAP_FAILED before use.
        let base = unsafe {
            libc::mmap(
                ptr::null_mut(),
                self.mapping_size,
                permissions.protection(),
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(base, libc::MAP_FAILED, "fixture mmap must succeed");
        let base = base.cast::<u8>();
        for segment in &self.segments {
            let destination_offset = usize::try_from(segment.virtual_address)
                .unwrap()
                .wrapping_sub(self.link_base);
            assert!(destination_offset + segment.memory_size <= self.mapping_size);
            // SAFETY: both source and destination ranges were bounds-checked;
            // each destination lies in this fresh private mapping.
            unsafe {
                ptr::copy_nonoverlapping(
                    image.as_ptr().add(segment.file_offset),
                    base.add(destination_offset),
                    segment.file_size,
                );
                base.add(destination_offset + segment.file_size)
                    .write_bytes(0, segment.memory_size - segment.file_size);
            }
        }
        LoadedElf {
            base,
            size: self.mapping_size,
        }
    }

    pub fn link_base(&self) -> usize {
        self.link_base
    }

    pub fn load_bias(&self, mapping: &LoadedElf) -> i128 {
        mapping.base_address() as i128 - self.link_base as i128
    }

    pub fn entry_address(&self, mapping: &LoadedElf) -> *mut u8 {
        mapping.base.wrapping_add(self.entry_offset)
    }

    pub fn runtime_address(&self, mapping: &LoadedElf, linked_address: usize) -> *mut u8 {
        let offset = linked_address
            .checked_sub(self.link_base)
            .expect("fixture symbol must not precede PT_LOAD");
        assert!(
            offset <= self.mapping_size,
            "fixture symbol must lie in PT_LOAD or point one byte past it"
        );
        mapping.base.wrapping_add(offset)
    }
}

pub struct LoadedElf {
    base: *mut u8,
    size: usize,
}

impl LoadedElf {
    pub fn base_address(&self) -> usize {
        self.base as usize
    }
}

impl Drop for LoadedElf {
    fn drop(&mut self) {
        // SAFETY: this is the exact live mapping returned by mmap and owned by
        // this value; no loaded function is running during drop.
        assert_eq!(unsafe { libc::munmap(self.base.cast(), self.size) }, 0);
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn align_up(value: usize, alignment: usize) -> usize {
    value
        .checked_add(alignment - 1)
        .expect("fixture size must not overflow")
        & !(alignment - 1)
}
