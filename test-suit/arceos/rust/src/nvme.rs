use ax_std::fs;

use crate::TestResult;

const NVME_FILE: &str = "/arceos-nvme-qemu.bin";
const NVME_BYTES: usize = 256 * 1024;

pub fn run() -> TestResult {
    verify_nvme_io()?;
    println!("NVME_QEMU_OK lba=4096 mdts=4");
    Ok(())
}

fn verify_nvme_io() -> TestResult {
    // Each word carries its file offset. A periodic pattern would miss
    // duplicated or reordered DMA pages and MDTS-sized chunks.
    let expected = (0..NVME_BYTES / 8)
        .flat_map(|index| (0xcafe_beef_0000_0000u64 | index as u64).to_le_bytes())
        .collect::<std::vec::Vec<_>>();
    let io_before = nvme_io_stats()?;

    let _ = fs::remove_file(NVME_FILE);
    fs::write(NVME_FILE, &expected).map_err(|_| "NVMe namespace file write failed")?;
    ax_fs_ng::file::sync_all_cached_files(false)
        .map_err(|_| "NVMe namespace file-cache writeback failed")?;
    // Force dirty block-cache folios through the real NVMe request queues.
    // The device fixture limits MDTS to 64 KiB, so each 128 KiB block-cache
    // writeback batch requires driver-side request splitting.
    ax_fs_ng::sync_all_block_caches().map_err(|_| "NVMe namespace writeback failed")?;
    let io_after_write = nvme_io_stats()?;
    let write_sectors = io_after_write
        .sectors_written
        .saturating_sub(io_before.sectors_written);
    if io_after_write.writes <= io_before.writes
        || write_sectors < (NVME_BYTES / 512) as u64
        || io_after_write.failed_requests != io_before.failed_requests
    {
        return Err("NVMe file writeback did not complete the expected device writes");
    }

    // Reclaim all now-clean file pages and block folios, then independently
    // require a full-file sector count from the cold read.
    let reclaimed = ax_fs_ng::file::page_cache_reclaim(usize::MAX);
    if reclaimed < NVME_BYTES / 4096 {
        return Err("NVMe readback could not reclaim enough clean cached pages");
    }
    let io_before_read = nvme_io_stats()?;

    let actual = fs::read(NVME_FILE).map_err(|_| "NVMe namespace file read failed")?;
    let io_after_read = nvme_io_stats()?;
    if actual != expected {
        return Err("NVMe namespace readback differed from the written pattern");
    }
    let read_sectors = io_after_read
        .sectors_read
        .saturating_sub(io_before_read.sectors_read);
    if io_after_read.reads <= io_before_read.reads
        || read_sectors < (NVME_BYTES / 512) as u64
        || io_after_read.failed_requests != io_before_read.failed_requests
    {
        return Err("NVMe file readback did not complete the expected device reads");
    }
    println!(
        "NVME_DEVICE_IO read_ios={} read_sectors={} write_ios={} write_sectors={}",
        io_after_read.reads.saturating_sub(io_before_read.reads),
        read_sectors,
        io_after_read.writes.saturating_sub(io_before.writes),
        write_sectors
    );

    fs::remove_file(NVME_FILE).map_err(|_| "NVMe namespace file cleanup failed")?;
    ax_fs_ng::file::sync_all_cached_files(false)
        .map_err(|_| "NVMe namespace cleanup file-cache writeback failed")?;
    ax_fs_ng::sync_all_block_caches().map_err(|_| "NVMe namespace cleanup writeback failed")?;
    Ok(())
}

fn nvme_io_stats() -> Result<ax_fs_ng::BlockIoStats, &'static str> {
    let (identity, stats) =
        ax_fs_ng::root::root_block_stats().ok_or("NVMe test requires a mounted block root")?;
    if identity.name != "nvme0n1" {
        return Err("NVMe test root is not the fixture namespace");
    }
    Ok(stats)
}
