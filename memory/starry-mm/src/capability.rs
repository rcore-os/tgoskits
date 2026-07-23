//! Capabilities supplied by the StarryOS kernel adapters.

use alloc::{string::String, sync::Arc};

use ax_errno::{AxError, AxResult};
use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr};

/// Stable file identity needed by Linux VMA reporting.
pub struct VmFileInfo {
    pub path: String,
    pub inode: u64,
    pub device: u64,
}

/// File operations required by a private file-backed mapping.
pub trait VmFile: Send + Sync {
    fn size_bytes(&self) -> AxResult<u64>;

    fn read_at(&self, buffer: &mut [u8], offset: u64) -> AxResult<usize>;

    fn info(&self) -> AxResult<VmFileInfo>;
}

/// Private file mapping supplied to the COW policy.
#[derive(Clone)]
pub struct PrivateFileMapping {
    file: Arc<dyn VmFile>,
    vaddr_base: VirtAddr,
    file_start: u64,
    file_end: Option<u64>,
}

impl PrivateFileMapping {
    pub fn new(
        file: Arc<dyn VmFile>,
        vaddr_base: VirtAddr,
        file_start: u64,
        file_end: Option<u64>,
    ) -> Self {
        Self {
            file,
            vaddr_base,
            file_start,
            file_end,
        }
    }

    pub const fn vaddr_base(&self) -> VirtAddr {
        self.vaddr_base
    }

    /// Reads the file-backed bytes for one zero-initialized resident page.
    pub fn read_page(&self, page_vaddr: VirtAddr, buffer: &mut [u8]) -> AxResult<usize> {
        let buffer_offset = self
            .vaddr_base
            .as_usize()
            .saturating_sub(page_vaddr.as_usize());
        if buffer_offset >= buffer.len() {
            return Err(AxError::InvalidInput);
        }
        let file_offset = self.file_start
            + page_vaddr
                .as_usize()
                .saturating_sub(self.vaddr_base.as_usize()) as u64;
        let read_len = self.max_read_len(file_offset, buffer.len() - buffer_offset)?;
        self.file.read_at(
            &mut buffer[buffer_offset..buffer_offset + read_len],
            file_offset,
        )
    }

    /// Reads a consecutive run that starts at or after the file VMA base.
    pub fn read_run(&self, start_vaddr: VirtAddr, buffer: &mut [u8]) -> AxResult<usize> {
        let vaddr_offset = start_vaddr
            .as_usize()
            .checked_sub(self.vaddr_base.as_usize())
            .ok_or(AxError::InvalidInput)?;
        let file_offset = self.file_start + vaddr_offset as u64;
        let read_len = self.max_read_len(file_offset, buffer.len())?;
        self.file.read_at(&mut buffer[..read_len], file_offset)
    }

    /// Returns file identity and the page-aligned offset reported for a VMA.
    pub fn info(&self, mapping_start: VirtAddr) -> AxResult<PrivateFileMappingInfo> {
        let info = self.file.info()?;
        let offset = self.file_start
            + mapping_start
                .as_usize()
                .saturating_sub(self.vaddr_base.as_usize()) as u64;
        Ok(PrivateFileMappingInfo {
            file: info,
            offset: offset & !(PAGE_SIZE_4K as u64 - 1),
        })
    }

    fn max_read_len(&self, file_offset: u64, available: usize) -> AxResult<usize> {
        let file_len = if self.file_end.is_none() {
            self.file.size_bytes()?
        } else {
            0
        };
        private_file_max_read_len(file_len, self.file_end, file_offset, available)
    }
}

pub struct PrivateFileMappingInfo {
    pub file: VmFileInfo,
    pub offset: u64,
}

fn private_file_max_read_len(
    file_len: u64,
    file_end: Option<u64>,
    file_offset: u64,
    available: usize,
) -> AxResult<usize> {
    let effective_end = match file_end {
        Some(end) => end,
        None => {
            if file_offset >= file_len {
                return Err(AxError::BadAddress);
            }
            file_len
        }
    };
    Ok(effective_end
        .saturating_sub(file_offset)
        .min(available as u64) as usize)
}

#[doc(hidden)]
pub fn private_file_eof_policy_matches_linux_for_test() -> bool {
    matches!(
        private_file_max_read_len(4096, None, 4096, 4096),
        Err(AxError::BadAddress)
    ) && matches!(private_file_max_read_len(4096, None, 2048, 4096), Ok(2048))
        && matches!(
            private_file_max_read_len(4096, Some(8192), 4096, 4096),
            Ok(4096)
        )
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    struct TestFile {
        bytes: alloc::vec::Vec<u8>,
    }

    impl VmFile for TestFile {
        fn size_bytes(&self) -> AxResult<u64> {
            Ok(self.bytes.len() as u64)
        }

        fn read_at(&self, buffer: &mut [u8], offset: u64) -> AxResult<usize> {
            let start = offset as usize;
            let end = start + buffer.len();
            buffer.copy_from_slice(&self.bytes[start..end]);
            Ok(buffer.len())
        }

        fn info(&self) -> AxResult<VmFileInfo> {
            Ok(VmFileInfo {
                path: "/test".into(),
                inode: 1,
                device: 2,
            })
        }
    }

    #[test]
    fn unaligned_first_page_keeps_the_prefix_zeroed() {
        let file = Arc::new(TestFile {
            bytes: vec![0x5a; PAGE_SIZE_4K],
        });
        let mapping = PrivateFileMapping::new(file, 0x1800.into(), 0, None);
        let mut page = vec![0; PAGE_SIZE_4K];

        assert_eq!(mapping.read_page(0x1000.into(), &mut page), Ok(0x800));
        assert!(page[..0x800].iter().all(|&byte| byte == 0));
        assert!(page[0x800..].iter().all(|&byte| byte == 0x5a));
    }

    #[test]
    fn private_mapping_rejects_a_fault_at_file_eof() {
        assert!(private_file_eof_policy_matches_linux_for_test());
    }
}
