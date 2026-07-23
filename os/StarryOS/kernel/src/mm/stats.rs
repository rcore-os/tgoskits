//! Adapter from Starry kernel VM objects to `starry-mm` statistics snapshots.

use alloc::string::String;

use ax_memory_addr::VirtAddr;
pub use starry_mm::ProcessMemStats;
use starry_mm::{ResidentSnapshot, StackRange, VmaPermissions};

use super::{AddrSpace, BackendFileInfo};

/// Collects Linux process memory statistics from one locked address space.
pub fn collect_process_mem_stats(aspace: &AddrSpace) -> ProcessMemStats {
    let mut stats = ProcessMemStats::default();
    let stack = StackRange {
        start: VirtAddr::from(
            crate::config::USER_STACK_TOP.saturating_sub(crate::config::USER_STACK_SIZE),
        ),
        end: VirtAddr::from(crate::config::USER_STACK_TOP),
    };
    for area in aspace.areas() {
        let file = area.backend().file_info().unwrap_or(BackendFileInfo {
            path: String::new(),
            offset: None,
            inode: None,
            dev: None,
            shared: false,
        });
        stats.record_vma(
            stack,
            (area.start(), area.end()),
            VmaPermissions {
                writable: area
                    .flags()
                    .contains(ax_runtime::hal::paging::MappingFlags::WRITE),
                executable: area
                    .flags()
                    .contains(ax_runtime::hal::paging::MappingFlags::EXECUTE),
            },
            &file.path,
            file.shared,
        );
    }

    let accounting = aspace.rss();
    let total_pages = accounting.rss_total_pages();
    accounting.sync_rss_atomics_from_charges();
    let (charged_anon, charged_file, charged_shmem) = accounting.snapshot_resident_charges();
    let file_only = accounting.rss_file_pages().saturating_sub(charged_file);
    let shmem_only = accounting.rss_shmem_pages().saturating_sub(charged_shmem);
    stats.finish(ResidentSnapshot {
        total_pages,
        anon_pages: charged_anon,
        file_pages: charged_file.saturating_add(file_only),
        shmem_pages: charged_shmem.saturating_add(shmem_only),
        hiwater_pages: accounting.hiwater_rss_pages(),
        peak_vss_pages: aspace.vm_stat.peak_vss_pages(),
    });
    stats
}
