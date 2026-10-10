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
    let io_before = ax_fs_ng::block_io_stats();

    let _ = fs::remove_file(NVME_FILE);
    fs::write(NVME_FILE, &expected).map_err(|_| "NVMe namespace file write failed")?;
    ax_fs_ng::file::sync_all_cached_files(false)
        .map_err(|_| "NVMe namespace file-cache writeback failed")?;
    // Force dirty block-cache folios through the real NVMe request queues.
    // The device fixture limits MDTS to 64 KiB, so each 128 KiB block-cache
    // writeback batch requires driver-side request splitting.
    ax_fs_ng::sync_all_block_caches().map_err(|_| "NVMe namespace writeback failed")?;
    let io_after_write = ax_fs_ng::block_io_stats();
    let write_sectors = io_after_write.3.saturating_sub(io_before.3);
    if io_after_write.2 <= io_before.2 || write_sectors < (NVME_BYTES / 512) as u64 {
        return Err("NVMe file writeback did not complete the expected device writes");
    }

    // Reclaim all now-clean file pages and block folios, then independently
    // require a full-file sector count from the cold read.
    let reclaimed = ax_fs_ng::file::page_cache_reclaim(usize::MAX);
    if reclaimed < NVME_BYTES / 4096 {
        return Err("NVMe readback could not reclaim enough clean cached pages");
    }
    let io_before_read = ax_fs_ng::block_io_stats();

    let actual = fs::read(NVME_FILE).map_err(|_| "NVMe namespace file read failed")?;
    let io_after_read = ax_fs_ng::block_io_stats();
    if actual != expected {
        return Err("NVMe namespace readback differed from the written pattern");
    }
    let read_sectors = io_after_read.1.saturating_sub(io_before_read.1);
    if io_after_read.0 <= io_before_read.0 || read_sectors < (NVME_BYTES / 512) as u64 {
        return Err("NVMe file readback did not complete the expected device reads");
    }
    println!(
        "NVME_DEVICE_IO read_ios={} read_sectors={} write_ios={} write_sectors={}",
        io_after_read.0.saturating_sub(io_before_read.0),
        read_sectors,
        io_after_read.2.saturating_sub(io_before.2),
        write_sectors
    );

    fs::remove_file(NVME_FILE).map_err(|_| "NVMe namespace file cleanup failed")?;
    ax_fs_ng::file::sync_all_cached_files(false)
        .map_err(|_| "NVMe namespace cleanup file-cache writeback failed")?;
    ax_fs_ng::sync_all_block_caches().map_err(|_| "NVMe namespace cleanup writeback failed")?;
    Ok(())
}
