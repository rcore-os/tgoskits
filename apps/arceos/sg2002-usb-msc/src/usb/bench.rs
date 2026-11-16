//! Existing read/write sampling shared by both execution entry points.
use alloc::vec;

use ax_hal::time::monotonic_time_nanos;
use ax_std as std;
use crab_usb::EndpointHandle;

use super::{
    BotTag, Capacity, SmokeError, bot_read10, bot_write10, checksum32, fill_write_pattern,
};
use crate::{
    BenchConfig, WriteBenchConfig, bench_iterations, blocks_per_transfer, mib_per_sec_x100,
};

#[derive(Clone, Copy)]
pub(super) struct BenchSummary {
    pub(super) bytes: usize,
    pub(super) nanos: u64,
    pub(super) checksum: u32,
}

pub(super) async fn run_read_bench(
    bulk_out: &mut EndpointHandle,
    bulk_in: &mut EndpointHandle,
    tag: &mut BotTag,
    capacity: Capacity,
    config: BenchConfig,
) -> Result<BenchSummary, SmokeError> {
    let mut total_bytes = 0usize;
    let mut total_nanos = 0u64;
    let mut total_checksum = 0u32;

    for size in config.read_sizes {
        let block_size = capacity.block_size as usize;
        let transfer_blocks = blocks_per_transfer(size, block_size);
        if transfer_blocks == 0 {
            continue;
        }
        let transfer_blocks = u64::from(transfer_blocks).min(capacity.blocks);
        if transfer_blocks == 0 {
            continue;
        }
        let transfer_bytes = transfer_blocks as usize * block_size;
        let max_iters = (capacity.blocks / transfer_blocks).max(1) as usize;
        let iters = bench_iterations(transfer_bytes).min(max_iters);
        let mut buffer = vec![0u8; transfer_bytes];
        let mut lba = 0u32;
        let start = monotonic_time_nanos();
        let mut bytes = 0usize;
        let mut checksum = 0u32;
        for _ in 0..iters {
            bot_read10(
                bulk_out,
                bulk_in,
                tag,
                lba,
                transfer_blocks as u16,
                &mut buffer,
            )
            .await?;
            checksum = checksum.wrapping_add(checksum32(&buffer));
            bytes = bytes.saturating_add(buffer.len());
            lba = lba.saturating_add(transfer_blocks as u32);
        }
        let nanos = monotonic_time_nanos().saturating_sub(start).max(1);
        total_bytes = total_bytes.saturating_add(bytes);
        total_nanos = total_nanos.saturating_add(nanos);
        total_checksum = total_checksum.wrapping_add(checksum);
        std::println!(
            "SG2002_DWC2_MSC_READ_PERF size={} blocks={} iters={} bytes={} ns={} mib_s_x100={} \
             checksum=0x{checksum:08x}",
            transfer_bytes,
            transfer_blocks,
            iters,
            bytes,
            nanos,
            mib_per_sec_x100(bytes, nanos)
        );
    }

    Ok(BenchSummary {
        bytes: total_bytes,
        nanos: total_nanos,
        checksum: total_checksum,
    })
}

pub(super) async fn run_write_bench(
    bulk_out: &mut EndpointHandle,
    bulk_in: &mut EndpointHandle,
    tag: &mut BotTag,
    capacity: Capacity,
    config: WriteBenchConfig,
) -> Result<BenchSummary, SmokeError> {
    let end_lba = u64::from(config.start_lba) + u64::from(config.blocks);
    if end_lba > capacity.blocks {
        return Err(SmokeError::InvalidWriteBench {
            start_lba: config.start_lba,
            blocks: config.blocks,
            capacity_blocks: capacity.blocks,
        });
    }

    let bytes = usize::from(config.blocks) * capacity.block_size as usize;
    let mut pattern = vec![0u8; bytes];
    fill_write_pattern(config.start_lba, &mut pattern);
    let expected_checksum = checksum32(&pattern);

    let start = monotonic_time_nanos();
    bot_write10(
        bulk_out,
        bulk_in,
        tag,
        config.start_lba,
        config.blocks,
        &pattern,
    )
    .await?;
    let nanos = monotonic_time_nanos().saturating_sub(start).max(1);

    let mut readback = vec![0u8; bytes];
    bot_read10(
        bulk_out,
        bulk_in,
        tag,
        config.start_lba,
        config.blocks,
        &mut readback,
    )
    .await?;
    let actual_checksum = checksum32(&readback);
    if actual_checksum != expected_checksum || readback != pattern {
        return Err(SmokeError::WriteVerifyMismatch {
            expected: expected_checksum,
            actual: actual_checksum,
        });
    }

    std::println!(
        "SG2002_DWC2_MSC_WRITE_PERF lba={} blocks={} size={} bytes={} ns={} mib_s_x100={} \
         checksum=0x{expected_checksum:08x}",
        config.start_lba,
        config.blocks,
        bytes,
        bytes,
        nanos,
        mib_per_sec_x100(bytes, nanos)
    );

    Ok(BenchSummary {
        bytes,
        nanos,
        checksum: expected_checksum,
    })
}
