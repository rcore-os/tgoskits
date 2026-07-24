//! Linux process memory statistics derived from VMA and RSS snapshots.

use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr};

const STACK_VMA_NAME: &str = "[stack]";
const HEAP_VMA_NAME: &str = "[heap]";

/// User stack address range used to classify unnamed stack VMAs.
#[derive(Debug, Clone, Copy)]
pub struct StackRange {
    pub start: VirtAddr,
    pub end: VirtAddr,
}

/// Resident counters supplied by the address-space owner.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResidentSnapshot {
    pub total_pages: u64,
    pub anon_pages: u64,
    pub file_pages: u64,
    pub shmem_pages: u64,
    pub hiwater_pages: u64,
    pub peak_vss_pages: u64,
}

/// Permission bits needed to classify a VMA without binding to a page-table implementation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VmaPermissions {
    pub writable: bool,
    pub executable: bool,
}

/// Per-process memory counters aggregated from VMA metadata.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessMemStats {
    pub vss_pages: u64,
    pub text_pages: u64,
    pub data_pages: u64,
    pub stack_pages: u64,
    pub exe_pages: u64,
    pub shared_vss_pages: u64,
    pub resident_pages: u64,
    pub peak_pages: u64,
    pub rss_anon_pages: u64,
    pub rss_file_pages: u64,
    pub rss_shmem_pages: u64,
    pub hiwater_rss_pages: u64,
    pub start_code: u64,
    pub end_code: u64,
    pub start_stack: u64,
}

impl ProcessMemStats {
    /// Adds one VMA snapshot to the aggregate.
    pub fn record_vma(
        &mut self,
        stack: StackRange,
        range: (VirtAddr, VirtAddr),
        permissions: VmaPermissions,
        path: &str,
        shared: bool,
    ) {
        let (start, end) = range;
        let pages = ((end - start) / PAGE_SIZE_4K) as u64;
        self.vss_pages += pages;
        if shared {
            self.shared_vss_pages += pages;
        }

        let is_stack = path == STACK_VMA_NAME || (stack.start <= start && start < stack.end);
        if is_stack {
            self.stack_pages += pages;
            if self.start_stack == 0 {
                self.start_stack = start.as_usize() as u64;
            }
        } else if permissions.executable {
            self.text_pages += pages;
            if !path.is_empty() && path != STACK_VMA_NAME && path != HEAP_VMA_NAME {
                self.exe_pages += pages;
            }
            let start = start.as_usize() as u64;
            let end = end.as_usize() as u64;
            if self.start_code == 0 || start < self.start_code {
                self.start_code = start;
            }
            self.end_code = self.end_code.max(end);
        } else if permissions.writable {
            self.data_pages += pages;
        }
    }

    /// Applies resident and watermark counters after the VMA walk.
    pub fn finish(&mut self, resident: ResidentSnapshot) {
        self.rss_anon_pages = resident.anon_pages;
        self.rss_file_pages = resident.file_pages;
        self.rss_shmem_pages = resident.shmem_pages;
        self.resident_pages = resident
            .anon_pages
            .saturating_add(resident.file_pages)
            .saturating_add(resident.shmem_pages)
            .max(resident.total_pages);
        self.hiwater_rss_pages = resident.hiwater_pages.max(self.resident_pages);
        self.peak_pages = resident.peak_vss_pages.max(self.vss_pages);
    }

    pub const fn vsize_bytes(&self) -> u64 {
        self.vss_pages * PAGE_SIZE_4K as u64
    }

    pub const fn rss_pages(&self) -> i64 {
        self.resident_pages as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STACK: StackRange = StackRange {
        start: VirtAddr::from_usize(0x8000),
        end: VirtAddr::from_usize(0xa000),
    };

    #[test]
    fn classifies_vma_snapshots() {
        let mut stats = ProcessMemStats::default();
        stats.record_vma(
            STACK,
            (0x8000.into(), 0xa000.into()),
            VmaPermissions {
                writable: true,
                executable: false,
            },
            "",
            false,
        );
        stats.record_vma(
            STACK,
            (0x1000.into(), 0x3000.into()),
            VmaPermissions {
                writable: false,
                executable: true,
            },
            "/bin/app",
            false,
        );
        stats.finish(ResidentSnapshot {
            anon_pages: 1,
            file_pages: 1,
            hiwater_pages: 3,
            peak_vss_pages: 8,
            ..Default::default()
        });

        assert_eq!(stats.vss_pages, 4);
        assert_eq!(stats.stack_pages, 2);
        assert_eq!(stats.text_pages, 2);
        assert_eq!(stats.resident_pages, 2);
        assert_eq!(stats.peak_pages, 8);
    }
}
