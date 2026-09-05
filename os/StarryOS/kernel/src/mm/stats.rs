//! Process memory statistics derived from VMA metadata and RSS accounting.

use alloc::{format, string::String};

use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr, VirtAddrRange};
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
    /// Writable data VMA pages excluding stack and pure executable regions
    /// (`VmData`; Linux `statm.data` adds `stack_pages` at read time).
    pub data_pages: u64,
    /// Stack VMA pages (`[stack]` name or USER_STACK range).
    pub stack_pages: u64,
    /// File-backed executable VMA pages (VmExe approximation).
    pub exe_pages: u64,
    /// Virtual page count of mappings whose backend reports `shared == true` (VSS).
    pub shared_vss_pages: u64,
    /// Resident set size in pages (`statm resident`, `stat` field 24, VmRSS).
    pub resident_pages: u64,
    /// Virtual pages covered by `VM_LOCKED` or `VM_LOCKONFAULT` VMAs.
    pub locked_pages: u64,
    /// Peak virtual address space in pages (VmPeak). Sourced from the
    /// per-process atomic watermark updated on every successful map.
    pub peak_pages: u64,
    /// Resident anonymous pages (from address-space RSS counters).
    pub rss_anon_pages: u64,
    /// Resident file-backed pages.
    pub rss_file_pages: u64,
    /// Resident shared-memory pages.
    pub rss_shmem_pages: u64,
    /// High-water RSS in pages (Linux `hiwater_rss`, read-side max with current).
    pub hiwater_rss_pages: u64,
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

fn user_stack_range(top: usize) -> (usize, usize) {
    let size = crate::config::USER_STACK_SIZE;
    (top.saturating_sub(size), top)
}

fn is_stack_vma(path: &str, start: VirtAddr, stack_top: usize) -> bool {
    if path == STACK_VMA_NAME {
        return true;
    }
    let (stack_start, stack_end) = user_stack_range(stack_top);
    let start = start.as_usize();
    start >= stack_start && start < stack_end
}

fn is_named_anon(path: &str) -> bool {
    path == STACK_VMA_NAME || path == HEAP_VMA_NAME
}

fn classify_vma(
    path: &str,
    flags: MappingFlags,
    start: VirtAddr,
    stack_top: usize,
) -> VmaClass {
    if is_stack_vma(path, start, stack_top) {
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
    range: VirtAddrRange,
    shared: bool,
    stack_top: usize,
) {
    let start = range.start;
    let end = range.end;
    stats.vss_pages += pages;
    if shared {
        stats.shared_vss_pages += pages;
    }

    let class = classify_vma(path, flags, start, stack_top);
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
    /// Current VSS / VMA breakdown comes from a VMA walk; VmPeak from
    /// [`AddrSpace::vm_stat`]; resident RSS from the published MappingSlot set.
    /// Metadata and allocation errors are returned without fabricating an empty VMA set.
    pub fn collect(aspace: &AddrSpace) -> crate::StarryResult<Self> {
        let mut stats = Self::default();
        let stack_top = aspace.stack_top().as_usize();
        for area in aspace.vma_inspection_records()? {
            let pages = (area.size() / PAGE_SIZE_4K) as u64;
            let flags = area.flags();
            let file_info = area.file_info();
            accumulate_vma(
                &mut stats,
                pages,
                &file_info.path,
                flags,
                VirtAddrRange::new(area.start(), area.end()),
                file_info.shared,
                stack_top,
            );
            if area.is_locked() {
                stats.locked_pages = stats.locked_pages.saturating_add(pages);
            }
        }
        let resident = aspace.resident_page_counts();
        stats.rss_anon_pages = resident.anon;
        stats.rss_file_pages = resident.file;
        stats.rss_shmem_pages = resident.shmem;
        stats.resident_pages = resident.total();
        stats.hiwater_rss_pages = aspace.resident_hiwater_pages();
        stats.peak_pages = aspace.vm_stat.peak_vss_pages().max(stats.vss_pages);
        Ok(stats)
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
    ///
    /// `shared` is Linux-like: resident file + shmem pages (`MM_FILEPAGES +
    /// MM_SHMEMPAGES`), not VSS or mapcount.
    pub fn format_statm(&self) -> String {
        let shared_rss = self.rss_file_pages + self.rss_shmem_pages;
        format!(
            "{} {} {} {} 0 {} 0\n",
            self.vss_pages,
            self.resident_pages,
            shared_rss,
            self.text_pages,
            self.data_pages.saturating_add(self.stack_pages),
        )
    }

    /// Render Vm* lines for `/proc/[pid]/status` (kB, Linux `task_mem` layout).
    pub fn format_status_vm_lines(&self) -> String {
        let page_kb = PAGE_SIZE_4K as u64 / 1024;
        let peak_kb = self.peak_pages * page_kb;
        let vss_kb = self.vss_pages * page_kb;
        let hwm_kb = self.hiwater_rss_pages * page_kb;
        let resident_kb = self.resident_pages * page_kb;
        let locked_kb = self.locked_pages * page_kb;
        let anon_kb = self.rss_anon_pages * page_kb;
        let file_kb = self.rss_file_pages * page_kb;
        let shmem_kb = self.rss_shmem_pages * page_kb;
        let data_kb = self.data_pages * page_kb;
        let stack_kb = self.stack_pages * page_kb;
        let exe_kb = self.exe_pages * page_kb;
        format!(
            "VmPeak:\t{peak_kb} kB\nVmSize:\t{vss_kb} kB\nVmLck:\t{locked_kb} kB\nVmPin:\t0 \
             kB\nVmHWM:\t{hwm_kb} kB\nVmRSS:\t{resident_kb} kB\nRssAnon:\t{anon_kb} \
             kB\nRssFile:\t{file_kb} kB\nRssShmem:\t{shmem_kb} kB\nVmData:\t{data_kb} \
             kB\nVmStk:\t{stack_kb} kB\nVmExe:\t{exe_kb} kB\nVmLib:\t0 kB\nVmPTE:\t0 \
             kB\nVmSwap:\t0 kB\n"
        )
    }
}

#[cfg(all(test, not(axtest)))]
fn stats_classify_and_accumulate_rules_hold_for_test() -> bool {
    let stack_top = crate::config::USER_STACK_TOP_MAX;
    // Heap is classified as Data (writable, non-stack, non-exec).
    matches!(
        classify_vma(
            HEAP_VMA_NAME,
            MappingFlags::READ | MappingFlags::WRITE,
            VirtAddr::from(0),
            stack_top,
        ),
        VmaClass::Data,
    )
    // Empty path + READ-only + non-EXEC + non-WRITE falls through to Other.
    && matches!(
        classify_vma("", MappingFlags::READ, VirtAddr::from(0), stack_top),
        VmaClass::Other,
    )
    // Stack takes precedence over EXEC/WRITE flag-based classification.
    && matches!(
        classify_vma(
            STACK_VMA_NAME,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE,
            VirtAddr::from(0),
            stack_top,
        ),
        VmaClass::Stack,
    )
    && {
        // Accumulating a shared executable file mapping bumps shared_vss_pages
        // and exe_pages, and updates start_code/end_code bounds.
        let mut stats = ProcessMemStats::default();
        accumulate_vma(
            &mut stats,
            2,
            "/bin/app",
            MappingFlags::READ | MappingFlags::EXECUTE,
            VirtAddrRange::new(VirtAddr::from(0x4000), VirtAddr::from(0x6000)),
            true,
            stack_top,
        );
        // Accumulating another text mapping expands start_code/end_code.
        accumulate_vma(
            &mut stats,
            1,
            "/lib/libc.so",
            MappingFlags::READ | MappingFlags::EXECUTE,
            VirtAddrRange::new(VirtAddr::from(0x1000), VirtAddr::from(0x2000)),
            false,
            stack_top,
        );
        stats.vss_pages == 3
            && stats.shared_vss_pages == 2
            && stats.text_pages == 3
            && stats.exe_pages == 3
            && stats.start_code == 0x1000
            && stats.end_code == 0x6000
    }
    && {
        // Accumulating an empty-named executable updates text_pages but leaves
        // exe_pages unchanged (anonymous executable mapping).
        let mut stats = ProcessMemStats::default();
        accumulate_vma(
            &mut stats,
            1,
            "",
            MappingFlags::READ | MappingFlags::EXECUTE,
            VirtAddrRange::new(VirtAddr::from(0x1000), VirtAddr::from(0x2000)),
            false,
            stack_top,
        );
        stats.text_pages == 1 && stats.exe_pages == 0
    }
    && {
        // Accumulating a stack VMA records start_stack on the first stack seen.
        let mut stats = ProcessMemStats::default();
        let (stack_start, _stack_end) = user_stack_range(stack_top);
        accumulate_vma(
            &mut stats,
            4,
            "",
            MappingFlags::READ | MappingFlags::WRITE,
            VirtAddrRange::new(
                VirtAddr::from(stack_start + PAGE_SIZE_4K),
                VirtAddr::from(stack_start + 5 * PAGE_SIZE_4K),
            ),
            false,
            stack_top,
        );
        stats.stack_pages == 4
            && stats.start_stack == (stack_start + PAGE_SIZE_4K) as u64
            && stats.start_code == 0
            && stats.end_code == 0
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    #[test]
    fn classify_stack_by_name() {
        assert_eq!(
            classify_vma(
                STACK_VMA_NAME,
                MappingFlags::READ | MappingFlags::WRITE,
                VirtAddr::from(0x1000),
                crate::config::USER_STACK_TOP_MAX,
            ),
            VmaClass::Stack,
        );
    }

    #[test]
    fn classify_stack_by_address_range() {
        let (stack_start, _) = user_stack_range(crate::config::USER_STACK_TOP_MAX);
        assert_eq!(
            classify_vma(
                "",
                MappingFlags::READ | MappingFlags::WRITE,
                VirtAddr::from(stack_start + PAGE_SIZE_4K),
                crate::config::USER_STACK_TOP_MAX,
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
                VirtAddr::from(0),
                crate::config::USER_STACK_TOP_MAX,
            ),
            VmaClass::Text,
        );
        assert_eq!(
            classify_vma(
                "",
                MappingFlags::READ | MappingFlags::WRITE,
                VirtAddr::from(0),
                crate::config::USER_STACK_TOP_MAX,
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
            VirtAddrRange::new(
                VirtAddr::from(
                    crate::config::USER_STACK_TOP_MAX - crate::config::USER_STACK_SIZE,
                ),
                VirtAddr::from(crate::config::USER_STACK_TOP_MAX),
            ),
            false,
            crate::config::USER_STACK_TOP_MAX,
        );
        accumulate_vma(
            &mut stats,
            2,
            "/bin/app",
            MappingFlags::READ | MappingFlags::EXECUTE,
            VirtAddrRange::new(VirtAddr::from(0x1000), VirtAddr::from(0x3000)),
            false,
            crate::config::USER_STACK_TOP_MAX,
        );
        accumulate_vma(
            &mut stats,
            3,
            HEAP_VMA_NAME,
            MappingFlags::READ | MappingFlags::WRITE,
            VirtAddrRange::new(
                VirtAddr::from(crate::config::USER_HEAP_BASE),
                VirtAddr::from(crate::config::USER_HEAP_BASE + 3 * PAGE_SIZE_4K),
            ),
            false,
            crate::config::USER_STACK_TOP_MAX,
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
            resident_pages: 30,
            rss_anon_pages: 20,
            rss_file_pages: 7,
            rss_shmem_pages: 3,
            hiwater_rss_pages: 30,
            ..Default::default()
        };
        assert_eq!(stats.format_statm(), "100 30 10 10 0 60 0\n");
    }

    #[test]
    fn format_status_vm_lines_use_kilobytes() {
        let stats = ProcessMemStats {
            vss_pages: 256,
            data_pages: 64,
            stack_pages: 32,
            exe_pages: 16,
            resident_pages: 48,
            peak_pages: 512,
            rss_anon_pages: 40,
            rss_file_pages: 4,
            rss_shmem_pages: 4,
            hiwater_rss_pages: 48,
            ..Default::default()
        };
        let lines = stats.format_status_vm_lines();
        assert!(lines.contains("VmPeak:\t2048 kB\n"));
        assert!(lines.contains("VmSize:\t1024 kB\n"));
        assert!(lines.contains("VmHWM:\t192 kB\n"));
        assert!(lines.contains("VmRSS:\t192 kB\n"));
        assert!(lines.contains("RssAnon:\t160 kB\n"));
        assert!(lines.contains("RssFile:\t16 kB\n"));
        assert!(lines.contains("RssShmem:\t16 kB\n"));
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

    #[test]
    fn stats_classify_and_accumulate_rules_hold() {
        assert!(stats_classify_and_accumulate_rules_hold_for_test());
    }
}
