use std::{
    cell::Cell,
    env,
    hint::black_box,
    time::{Duration, Instant},
};

use rsext4::{
    BLOCK_SIZE, BlockDevice, Ext4Error, Ext4Result, Ext4Timestamp, Jbd2Dev, bmalloc::AbsoluteBN,
    mkfile, mkfs, mount, write_inode_data,
};

const DEFAULT_BYTES: usize = 20 * 1024 * 1024;
const DEFAULT_WARMUPS: usize = 3;
const DEFAULT_RUNS: usize = 20;
const IMAGE_BYTES: usize = 128 * 1024 * 1024;

struct MemoryDevice {
    bytes: Vec<u8>,
    seconds: Cell<i64>,
}

impl MemoryDevice {
    fn new() -> Self {
        Self {
            bytes: vec![0; IMAGE_BYTES],
            seconds: Cell::new(1_700_000_000),
        }
    }
}

impl BlockDevice for MemoryDevice {
    fn write(&mut self, buffer: &[u8], block: AbsoluteBN, _count: u32) -> Ext4Result<()> {
        let start = block.as_usize()? * BLOCK_SIZE;
        let end = start
            .checked_add(buffer.len())
            .ok_or_else(Ext4Error::invalid_input)?;
        let target = self
            .bytes
            .get_mut(start..end)
            .ok_or_else(Ext4Error::invalid_input)?;
        target.copy_from_slice(buffer);
        Ok(())
    }

    fn read(&mut self, buffer: &mut [u8], block: AbsoluteBN, _count: u32) -> Ext4Result<()> {
        let start = block.as_usize()? * BLOCK_SIZE;
        let end = start
            .checked_add(buffer.len())
            .ok_or_else(Ext4Error::invalid_input)?;
        let source = self
            .bytes
            .get(start..end)
            .ok_or_else(Ext4Error::invalid_input)?;
        buffer.copy_from_slice(source);
        Ok(())
    }

    fn open(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn close(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn total_blocks(&self) -> u64 {
        (self.bytes.len() / BLOCK_SIZE) as u64
    }

    fn block_size(&self) -> u32 {
        BLOCK_SIZE as u32
    }

    fn flush(&mut self) -> Ext4Result<()> {
        black_box(&self.bytes);
        Ok(())
    }

    fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
        let seconds = self.seconds.get();
        self.seconds.set(seconds + 1);
        Ok(Ext4Timestamp::new(seconds, 0))
    }
}

#[derive(Clone, Copy)]
struct Sample {
    dirty_sync: Duration,
    clean_sync: Duration,
    unmount: Duration,
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn run_once(payload: &[u8]) -> Sample {
    let mut device = Jbd2Dev::initial_jbd2dev(0, MemoryDevice::new(), true);
    mkfs(&mut device).expect("benchmark mkfs must succeed");
    let mut filesystem = mount(&mut device).expect("benchmark mount must succeed");
    mkfile(
        &mut device,
        &mut filesystem,
        "/rsext4-host-sync.bin",
        None,
        None,
    )
    .expect("benchmark create must succeed");
    let inode =
        rsext4::dir::get_inode_with_num(&mut filesystem, &mut device, "/rsext4-host-sync.bin")
            .expect("benchmark lookup must succeed")
            .expect("benchmark inode must exist")
            .0;
    write_inode_data(&mut device, &mut filesystem, inode, 0, payload)
        .expect("benchmark write must succeed");

    let start = Instant::now();
    filesystem
        .sync_filesystem(&mut device)
        .expect("benchmark dirty sync must succeed");
    let dirty_sync = start.elapsed();

    let start = Instant::now();
    filesystem
        .sync_filesystem(&mut device)
        .expect("benchmark clean sync must succeed");
    let clean_sync = start.elapsed();

    let start = Instant::now();
    filesystem
        .umount(&mut device)
        .expect("benchmark unmount must succeed");
    let unmount = start.elapsed();

    Sample {
        dirty_sync,
        clean_sync,
        unmount,
    }
}

fn percentile(samples: &[u128], numerator: usize, denominator: usize) -> u128 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = sorted
        .len()
        .saturating_mul(numerator)
        .div_ceil(denominator)
        .saturating_sub(1);
    sorted[rank.min(sorted.len() - 1)]
}

fn main() {
    let bytes = env_usize("RSEXT4_BENCH_BYTES", DEFAULT_BYTES);
    let warmups = env_usize("RSEXT4_BENCH_WARMUPS", DEFAULT_WARMUPS);
    let runs = env_usize("RSEXT4_BENCH_RUNS", DEFAULT_RUNS);
    assert!(bytes > 0 && bytes.is_multiple_of(BLOCK_SIZE));
    assert!(runs > 0);

    let mut payload = vec![0u8; bytes];
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(31).wrapping_add(7);
    }

    println!(
        "RSEXT4_BENCH_CONFIG commit=6e27704c4 arch=x86_64 backend=memory \
         feature=metadata_csum+64bit+journal workload=sync-cycle bytes={bytes} warmups={warmups} \
         runs={runs} block_size={BLOCK_SIZE} journal=true"
    );
    for _ in 0..warmups {
        black_box(run_once(&payload));
    }

    let mut samples = Vec::with_capacity(runs);
    for run in 0..runs {
        let sample = run_once(&payload);
        println!(
            "RSEXT4_BENCH_RESULT commit=6e27704c4 arch=x86_64 backend=memory \
             feature=metadata_csum+64bit+journal workload=sync-cycle run={run} dirty_sync_ns={} \
             clean_sync_ns={} unmount_ns={}",
            sample.dirty_sync.as_nanos(),
            sample.clean_sync.as_nanos(),
            sample.unmount.as_nanos()
        );
        samples.push(sample);
    }

    let dirty_sync = samples
        .iter()
        .map(|sample| sample.dirty_sync.as_nanos())
        .collect::<Vec<_>>();
    let clean_sync = samples
        .iter()
        .map(|sample| sample.clean_sync.as_nanos())
        .collect::<Vec<_>>();
    let unmount = samples
        .iter()
        .map(|sample| sample.unmount.as_nanos())
        .collect::<Vec<_>>();
    println!(
        "RSEXT4_BENCH_SUMMARY commit=6e27704c4 arch=x86_64 backend=memory \
         feature=metadata_csum+64bit+journal workload=sync-cycle dirty_sync_median_ns={} \
         dirty_sync_p95_ns={} clean_sync_median_ns={} clean_sync_p95_ns={} unmount_median_ns={} \
         unmount_p95_ns={}",
        percentile(&dirty_sync, 1, 2),
        percentile(&dirty_sync, 95, 100),
        percentile(&clean_sync, 1, 2),
        percentile(&clean_sync, 95, 100),
        percentile(&unmount, 1, 2),
        percentile(&unmount, 95, 100),
    );
}
