//! Process memory statistics derived from VMA metadata.
//!
//! Plan1 collects virtual-size metrics by iterating mapped areas without a
//! page-table walk. Real RSS accounting (populate/unmap counters or PTE walk)
//! is deferred to Plan2; until then see the `FIXME(plan2-rss)` in [`Self::collect`].

use alloc::{format, string::String};

use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr};
use ax_runtime::hal::paging::MappingFlags;

use super::AddrSpace;

const STACK_VMA_NAME: &str = "[stack]";
const HEAP_VMA_NAME: &str = "[heap]";

/// Per-process memory counters aggregated from VMA metadata.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessMemStats {
    /// Total virtual size in pages (sum of all VMA sizes).
    pub vss_pages: u64,
    /// Executable VMA pages excluding the stack mapping.
    pub text_pages: u64,
    /// Writable data VMA pages excluding stack and pure executable regions.
    pub data_pages: u64,
    /// Stack VMA pages (`[stack]` name or USER_STACK range).
    pub stack_pages: u64,
    /// File-backed executable VMA pages (VmExe approximation).
    pub exe_pages: u64,
    /// Virtual page count of mappings whose backend reports `shared == true`.
    ///
    /// This feeds `/proc/[pid]/statm` field 3 (`shared`). It is **not** Linux
    /// resident shared memory (no mapcount/PTE walk): it only sums the virtual
    /// size of MAP_SHARED / memfd-shared / `SharedBackend` VMAs, matching the
    /// coarse Linux approximation where `statm shared` counts file+shmem
    /// resident pages rather than true proportional sharing.
    pub shared_vss_pages: u64,
    /// Resident set size in pages (`statm resident`, `stat` field 24, VmRSS).
    ///
    /// Invariant: `resident_pages <= vss_pages`. Plan2 should populate this from
    /// incremental counters or a PTE walk; Plan1 temporarily mirrors VSS (see
    /// `collect()` FIXME).
    pub resident_pages: u64,
    /// Peak virtual address space in pages (VmPeak). Sourced from the
    /// per-process atomic watermark updated on every successful map.
    pub peak_pages: u64,
    /// Peak resident set size in pages (VmHWM). Sourced from the
    /// per-process atomic watermark updated on every successful map.
    pub hwm_pages: u64,
    /// Lowest executable mapping start (stat `start_code`).
    pub start_code: u64,
    /// Highest executable mapping end (stat `end_code`).
    pub end_code: u64,
    /// Stack region start (stat `start_stack`).
    pub start_stack: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VmaClass {
    Stack,
    Text,
    Data,
    Other,
}

fn user_stack_range() -> (usize, usize) {
    let top = crate::config::USER_STACK_TOP;
    let size = crate::config::USER_STACK_SIZE;
    (top.saturating_sub(size), top)
}

fn is_stack_vma(path: &str, start: VirtAddr) -> bool {
    if path == STACK_VMA_NAME {
        return true;
    }
    let (stack_start, stack_end) = user_stack_range();
    let start = start.as_usize();
    start >= stack_start && start < stack_end
}

fn is_named_anon(path: &str) -> bool {
    path == STACK_VMA_NAME || path == HEAP_VMA_NAME
}

fn classify_vma(path: &str, flags: MappingFlags, start: VirtAddr) -> VmaClass {
    if is_stack_vma(path, start) {
        return VmaClass::Stack;
    }
    if flags.contains(MappingFlags::EXECUTE) {
        return VmaClass::Text;
    }
    if flags.contains(MappingFlags::WRITE) {
        return VmaClass::Data;
    }
    VmaClass::Other
}

fn accumulate_vma(
    stats: &mut ProcessMemStats,
    pages: u64,
    path: &str,
    flags: MappingFlags,
    start: VirtAddr,
    end: VirtAddr,
    shared: bool,
) {
    stats.vss_pages += pages;
    if shared {
        stats.shared_vss_pages += pages;
    }

    let class = classify_vma(path, flags, start);
    match class {
        VmaClass::Stack => stats.stack_pages += pages,
        VmaClass::Text => {
            stats.text_pages += pages;
            if !path.is_empty() && !is_named_anon(path) {
                stats.exe_pages += pages;
            }
            let start = start.as_usize() as u64;
            let end = end.as_usize() as u64;
            if stats.start_code == 0 || start < stats.start_code {
                stats.start_code = start;
            }
            if end > stats.end_code {
                stats.end_code = end;
            }
        }
        VmaClass::Data => stats.data_pages += pages,
        VmaClass::Other => {}
    }

    if class == VmaClass::Stack && stats.start_stack == 0 {
        stats.start_stack = start.as_usize() as u64;
    }
}

impl ProcessMemStats {
    /// Collect memory statistics by iterating the address-space VMA list.
    ///
    /// Current VSS / VMA breakdown comes from a VMA walk; watermarks are
    /// read directly from [`AddrSpace::vm_stat`] in O(1).
    pub fn collect(aspace: &AddrSpace) -> Self {
        let mut stats = Self::default();
        for area in aspace.areas() {
            let pages = (area.size() / PAGE_SIZE_4K) as u64;
            let flags = area.flags();
            let file_info = area
                .backend()
                .file_info()
                .unwrap_or(super::BackendFileInfo {
                    path: String::new(),
                    offset: None,
                    inode: None,
                    dev: None,
                    shared: false,
                });
            accumulate_vma(
                &mut stats,
                pages,
                &file_info.path,
                flags,
                area.start(),
                area.end(),
                file_info.shared,
            );
        }
        // FIXME(plan2-rss): replace with real RSS from mm counters or PTE walk.
        stats.resident_pages = stats.vss_pages;
        // Watermarks come from the O(1) atomic counters in vm_stat; the
        // .max(current) floor handles the first collect after a fresh fork
        // before any new mapping has updated the child's watermarks.
        stats.peak_pages = aspace.vm_stat.peak_vss_pages().max(stats.vss_pages);
        stats.hwm_pages = aspace.vm_stat.peak_rss_pages().max(stats.resident_pages);
        stats
    }

    /// Virtual size in bytes (`stat` field 23).
    pub const fn vsize_bytes(&self) -> u64 {
        self.vss_pages * PAGE_SIZE_4K as u64
    }

    /// Resident set size in pages (`stat` field 24).
    pub const fn rss_pages(&self) -> i64 {
        self.resident_pages as i64
    }

    /// Render `/proc/[pid]/statm` (size resident shared text lib data dirty).
    pub fn format_statm(&self) -> String {
        format!(
            "{} {} {} {} 0 {} 0\n",
            self.vss_pages,
            self.resident_pages,
            self.shared_vss_pages,
            self.text_pages,
            self.data_pages,
        )
    }

    /// Render Vm* lines for `/proc/[pid]/status` (kB, Linux `task_mem` layout).
    pub fn format_status_vm_lines(&self) -> String {
        let page_kb = PAGE_SIZE_4K as u64 / 1024;
        let peak_kb = self.peak_pages * page_kb;
        let vss_kb = self.vss_pages * page_kb;
        let hwm_kb = self.hwm_pages * page_kb;
        let resident_kb = self.resident_pages * page_kb;
        let data_kb = self.data_pages * page_kb;
        let stack_kb = self.stack_pages * page_kb;
        let exe_kb = self.exe_pages * page_kb;
        format!(
            "VmPeak:\t{peak_kb} kB\nVmSize:\t{vss_kb} kB\nVmLck:\t0 kB\nVmPin:\t0 \
             kB\nVmHWM:\t{hwm_kb} kB\nVmRSS:\t{resident_kb} kB\nRssAnon:\t0 kB\nRssFile:\t0 \
             kB\nRssShmem:\t0 kB\nVmData:\t{data_kb} kB\nVmStk:\t{stack_kb} kB\nVmExe:\t{exe_kb} \
             kB\nVmLib:\t0 kB\nVmPTE:\t0 kB\nVmSwap:\t0 kB\n"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_stack_by_name() {
        assert_eq!(
            classify_vma(
                STACK_VMA_NAME,
                MappingFlags::READ | MappingFlags::WRITE,
                VirtAddr::from(0x1000),
            ),
            VmaClass::Stack,
        );
    }

    #[test]
    fn classify_stack_by_address_range() {
        let (stack_start, _) = user_stack_range();
        assert_eq!(
            classify_vma(
                "",
                MappingFlags::READ | MappingFlags::WRITE,
                VirtAddr::from(stack_start + PAGE_SIZE_4K),
            ),
            VmaClass::Stack,
        );
    }

    #[test]
    fn classify_text_and_data() {
        assert_eq!(
            classify_vma(
                "",
                MappingFlags::READ | MappingFlags::EXECUTE,
                VirtAddr::from(0)
            ),
            VmaClass::Text,
        );
        assert_eq!(
            classify_vma(
                "",
                MappingFlags::READ | MappingFlags::WRITE,
                VirtAddr::from(0)
            ),
            VmaClass::Data,
        );
    }

    #[test]
    fn accumulate_mixed_vmas() {
        let mut stats = ProcessMemStats::default();
        accumulate_vma(
            &mut stats,
            4,
            STACK_VMA_NAME,
            MappingFlags::READ | MappingFlags::WRITE,
            VirtAddr::from(crate::config::USER_STACK_TOP - crate::config::USER_STACK_SIZE),
            VirtAddr::from(crate::config::USER_STACK_TOP),
            false,
        );
        accumulate_vma(
            &mut stats,
            2,
            "/bin/app",
            MappingFlags::READ | MappingFlags::EXECUTE,
            VirtAddr::from(0x1000),
            VirtAddr::from(0x3000),
            false,
        );
        accumulate_vma(
            &mut stats,
            3,
            HEAP_VMA_NAME,
            MappingFlags::READ | MappingFlags::WRITE,
            VirtAddr::from(crate::config::USER_HEAP_BASE),
            VirtAddr::from(crate::config::USER_HEAP_BASE + 3 * PAGE_SIZE_4K),
            false,
        );

        assert_eq!(stats.vss_pages, 9);
        assert_eq!(stats.stack_pages, 4);
        assert_eq!(stats.text_pages, 2);
        assert_eq!(stats.exe_pages, 2);
        assert_eq!(stats.data_pages, 3);
        assert_eq!(stats.start_code, 0x1000);
        assert_eq!(stats.end_code, 0x3000);
    }

    #[test]
    fn format_statm_matches_linux_field_order() {
        let stats = ProcessMemStats {
            vss_pages: 100,
            text_pages: 10,
            data_pages: 40,
            stack_pages: 20,
            exe_pages: 8,
            shared_vss_pages: 5,
            resident_pages: 100,
            ..Default::default()
        };
        assert_eq!(stats.format_statm(), "100 100 5 10 0 40 0\n");
    }

    #[test]
    fn format_status_vm_lines_use_kilobytes() {
        let stats = ProcessMemStats {
            vss_pages: 256,
            data_pages: 64,
            stack_pages: 32,
            exe_pages: 16,
            resident_pages: 256,
            peak_pages: 512,
            hwm_pages: 256,
            ..Default::default()
        };
        let lines = stats.format_status_vm_lines();
        assert!(lines.contains("VmPeak:\t2048 kB\n"));
        assert!(lines.contains("VmSize:\t1024 kB\n"));
        assert!(lines.contains("VmHWM:\t1024 kB\n"));
        assert!(lines.contains("VmRSS:\t1024 kB\n"));
        assert!(lines.contains("VmData:\t256 kB\n"));
        assert!(lines.contains("VmStk:\t128 kB\n"));
        assert!(lines.contains("VmExe:\t64 kB\n"));
    }

    #[test]
    fn resident_never_exceeds_vss() {
        let stats = ProcessMemStats {
            vss_pages: 42,
            resident_pages: 30,
            ..Default::default()
        };
        assert!(stats.resident_pages <= stats.vss_pages);
        assert_eq!(stats.rss_pages(), 30);
        assert_eq!(stats.vsize_bytes(), 42 * PAGE_SIZE_4K as u64);
    }
}
