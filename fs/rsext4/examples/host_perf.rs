//! Reproducible host-side baseline for the existing rsext4 data path.

use std::{
    cell::Cell,
    env,
    hint::black_box,
    time::{Duration, Instant},
};

use rsext4::{
    BLOCK_SIZE, BlockIo, DirectoryCursor, Ext4, Ext4Error, Ext4Result, Ext4Timestamp, FileName,
    FilePermissions, InodeFlags, MkfsOptions, MountOptions, MountServices, MutationContext,
    NoopObserver, XattrNamespace, XattrSetMode, format,
};

const DEFAULT_BYTES: usize = 20 * 1024 * 1024;
const DEFAULT_WARMUPS: usize = 3;
const DEFAULT_RUNS: usize = 10;
const DEFAULT_XATTR_BYTES: usize = 512;
const DEFAULT_HTREE_ENTRIES: usize = 800;
const DEFAULT_HTREE_BATCH_ENTRIES: usize = 128;
const IMAGE_BYTES: usize = 128 * 1024 * 1024;

struct MemoryDevice {
    bytes: Vec<u8>,
}

impl MemoryDevice {
    fn new() -> Self {
        Self {
            bytes: vec![0; IMAGE_BYTES],
        }
    }
}

impl BlockIo for MemoryDevice {
    fn write(&mut self, buffer: &[u8], sector: rsext4::SectorId, _count: u32) -> Ext4Result<()> {
        let start = sector.as_usize()? * BLOCK_SIZE;
        let total_blocks = self.geometry().block_count;
        let end = start
            .checked_add(buffer.len())
            .ok_or_else(Ext4Error::invalid_input)?;
        let dst = self.bytes.get_mut(start..end).ok_or_else(|| {
            Ext4Error::block_out_of_range(sector.to_u32().unwrap_or(u32::MAX), total_blocks)
        })?;
        dst.copy_from_slice(buffer);
        Ok(())
    }

    fn read(&mut self, buffer: &mut [u8], sector: rsext4::SectorId, _count: u32) -> Ext4Result<()> {
        let start = sector.as_usize()? * BLOCK_SIZE;
        let total_blocks = self.geometry().block_count;
        let end = start
            .checked_add(buffer.len())
            .ok_or_else(Ext4Error::invalid_input)?;
        let src = self.bytes.get(start..end).ok_or_else(|| {
            Ext4Error::block_out_of_range(sector.to_u32().unwrap_or(u32::MAX), total_blocks)
        })?;
        buffer.copy_from_slice(src);
        Ok(())
    }

    fn flush(&mut self) -> Ext4Result<()> {
        black_box(&self.bytes);
        Ok(())
    }

    fn geometry(&self) -> rsext4::DeviceGeometry {
        rsext4::DeviceGeometry::new(BLOCK_SIZE as u32, {
            (self.bytes.len() / BLOCK_SIZE) as u64
        })
    }

    fn capabilities(&self) -> rsext4::DeviceCapabilities {
        rsext4::DeviceCapabilities {
            read_only: { false },

            flush: true,

            ..rsext4::DeviceCapabilities::default()
        }
    }
}

struct BenchClock(Cell<i64>);

impl rsext4::Clock for BenchClock {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        let seconds = self.0.get();
        self.0.set(seconds + 1);
        Ok(Ext4Timestamp::new(seconds, 0))
    }
}

#[derive(Clone, Copy)]
struct Sample {
    write: Duration,
    read: Duration,
    sync: Duration,
}

#[derive(Clone, Copy)]
struct XattrSample {
    set_sync: Duration,
    get: Duration,
    remove_sync: Duration,
}

#[derive(Clone, Copy)]
struct SyncCycleSample {
    dirty_sync: Duration,
    clean_sync: Duration,
    unmount: Duration,
}

#[derive(Clone, Copy)]
struct HTreeReadDirSample {
    readdir: Duration,
    entries: usize,
    calls: usize,
}

#[derive(Clone, Copy)]
struct BenchmarkConfig<'a> {
    commit: &'a str,
    arch: &'a str,
    backend: &'a str,
    feature: &'a str,
    warmups: usize,
    runs: usize,
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn marker_value(name: &str, default: &str) -> String {
    let value = env::var(name).unwrap_or_else(|_| default.to_string());
    assert!(
        !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:+".contains(&byte)),
        "{name} must be a non-empty marker token"
    );
    value
}

fn run_once(payload: &[u8]) -> Sample {
    let device = MemoryDevice::new();
    let device = format(
        device,
        BenchClock(Cell::new(1_700_000_000)),
        MkfsOptions::default(),
    )
    .expect("benchmark mkfs must succeed");
    let services = MountServices::new(BenchClock(Cell::new(1_700_000_000)), (), NoopObserver);
    let mut filesystem = Ext4::mount(device, services, MountOptions::read_write())
        .expect("benchmark mount must succeed");
    let context = MutationContext::new(0, 0, 0, 0);
    let file = filesystem
        .create_regular_file(
            context,
            filesystem.root_inode(),
            FileName::new(b"rsext4-host-perf.bin").expect("benchmark name must be valid"),
            FilePermissions::new(0o644).expect("benchmark permissions must be valid"),
        )
        .expect("benchmark file creation must succeed");

    let start = Instant::now();
    filesystem
        .write_inode(file.number, 0, payload)
        .expect("benchmark write must succeed");
    let write = start.elapsed();

    let start = Instant::now();
    let mut read_back = vec![0; payload.len()];
    let read = filesystem
        .read_inode(file.number, 0, &mut read_back)
        .expect("benchmark read must succeed");
    assert_eq!(read, read_back.len());
    black_box(&read_back);
    assert_eq!(read_back, payload);
    let read = start.elapsed();

    let start = Instant::now();
    filesystem
        .unmount()
        .expect("benchmark unmount must succeed");
    let sync = start.elapsed();

    Sample { write, read, sync }
}

fn run_xattr_once(value: &[u8]) -> XattrSample {
    let device = MemoryDevice::new();
    let device = format(
        device,
        BenchClock(Cell::new(1_700_000_000)),
        MkfsOptions::default(),
    )
    .expect("benchmark mkfs must succeed");
    let services = MountServices::new(BenchClock(Cell::new(1_700_000_000)), (), NoopObserver);
    let mut filesystem = Ext4::mount(device, services, MountOptions::read_write())
        .expect("benchmark mount must succeed");
    let context = MutationContext::new(0, 0, 0, 0);
    let file = filesystem
        .create_regular_file(
            context,
            filesystem.root_inode(),
            FileName::new(b"rsext4-host-xattr.bin").expect("benchmark name must be valid"),
            FilePermissions::new(0o644).expect("benchmark permissions must be valid"),
        )
        .expect("benchmark file creation must succeed");

    let start = Instant::now();
    filesystem
        .set_xattr(
            file.number,
            XattrNamespace::User,
            b"host-perf",
            value,
            XattrSetMode::Create,
        )
        .expect("benchmark xattr create must succeed");
    filesystem
        .sync()
        .expect("benchmark xattr create sync must succeed");
    let set_sync = start.elapsed();

    let start = Instant::now();
    let observed = filesystem
        .get_xattr(file.number, XattrNamespace::User, b"host-perf")
        .expect("benchmark xattr read must succeed");
    black_box(&observed);
    assert_eq!(observed, value);
    let get = start.elapsed();

    let start = Instant::now();
    filesystem
        .remove_xattr(file.number, XattrNamespace::User, b"host-perf")
        .expect("benchmark xattr remove must succeed");
    filesystem
        .sync()
        .expect("benchmark xattr remove sync must succeed");
    let remove_sync = start.elapsed();

    filesystem
        .unmount()
        .expect("benchmark unmount must succeed");
    XattrSample {
        set_sync,
        get,
        remove_sync,
    }
}

fn run_sync_cycle_once(payload: &[u8]) -> SyncCycleSample {
    let device = MemoryDevice::new();
    let device = format(
        device,
        BenchClock(Cell::new(1_700_000_000)),
        MkfsOptions::default(),
    )
    .expect("benchmark mkfs must succeed");
    let services = MountServices::new(BenchClock(Cell::new(1_700_000_000)), (), NoopObserver);
    let mut filesystem = Ext4::mount(device, services, MountOptions::read_write())
        .expect("benchmark mount must succeed");
    let context = MutationContext::new(0, 0, 0, 0);
    let file = filesystem
        .create_regular_file(
            context,
            filesystem.root_inode(),
            FileName::new(b"rsext4-host-sync.bin").expect("benchmark name must be valid"),
            FilePermissions::new(0o644).expect("benchmark permissions must be valid"),
        )
        .expect("benchmark file creation must succeed");
    filesystem
        .write_inode(file.number, 0, payload)
        .expect("benchmark write must succeed");

    let start = Instant::now();
    filesystem
        .sync()
        .expect("benchmark dirty sync must succeed");
    let dirty_sync = start.elapsed();

    let start = Instant::now();
    filesystem
        .sync()
        .expect("benchmark clean sync must succeed");
    let clean_sync = start.elapsed();

    let start = Instant::now();
    filesystem
        .unmount()
        .expect("benchmark unmount must succeed");
    let unmount = start.elapsed();

    SyncCycleSample {
        dirty_sync,
        clean_sync,
        unmount,
    }
}

fn run_htree_readdir_once(entry_count: usize, batch_entries: usize) -> HTreeReadDirSample {
    let device = MemoryDevice::new();
    let device = format(
        device,
        BenchClock(Cell::new(1_700_000_000)),
        MkfsOptions::default(),
    )
    .expect("benchmark mkfs must succeed");
    let services = MountServices::new(BenchClock(Cell::new(1_700_000_000)), (), NoopObserver);
    let mut filesystem = Ext4::mount(device, services, MountOptions::read_write())
        .expect("benchmark mount must succeed");
    let context = MutationContext::new(0, 0, 0, 0);
    let directory = filesystem
        .create_directory(
            context,
            filesystem.root_inode(),
            FileName::new(b"rsext4-host-htree").expect("benchmark name must be valid"),
            FilePermissions::new(0o755).expect("benchmark permissions must be valid"),
        )
        .expect("benchmark directory creation must succeed");
    let permissions = FilePermissions::new(0o644).expect("benchmark permissions must be valid");
    for index in 0..entry_count {
        let name = format!("entry-{index:08}.bin");
        filesystem
            .create_regular_file(
                context,
                directory.number,
                FileName::new(name.as_bytes()).expect("benchmark entry name must be valid"),
                permissions,
            )
            .expect("benchmark entry creation must succeed");
    }
    assert!(
        filesystem
            .inode(directory.number)
            .expect("benchmark directory inspection must succeed")
            .flags
            .contains(InodeFlags::DIRECTORY_INDEX),
        "benchmark fixture must use an HTree"
    );

    let start = Instant::now();
    let mut cursor = DirectoryCursor::Start;
    let mut entries = 0;
    let mut calls = 0;
    let mut reader = filesystem
        .open_directory_reader(directory.number)
        .expect("benchmark directory reader must open");
    loop {
        let batch = filesystem
            .read_directory_with_reader(&mut reader, cursor, batch_entries)
            .expect("benchmark HTree readdir must succeed");
        calls += 1;
        if batch.is_empty() {
            break;
        }
        for entry in batch {
            black_box(&entry.name);
            entries += 1;
            cursor = entry.next_cursor;
        }
        if cursor == DirectoryCursor::End {
            break;
        }
    }
    let readdir = start.elapsed();
    assert_eq!(entries, entry_count + 2);
    filesystem
        .unmount()
        .expect("benchmark unmount must succeed");
    HTreeReadDirSample {
        readdir,
        entries,
        calls,
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

fn run_xattr_benchmark(config: BenchmarkConfig<'_>, value_bytes: usize) {
    let BenchmarkConfig {
        commit,
        arch,
        backend,
        feature,
        warmups,
        runs,
    } = config;
    let mut value = vec![0u8; value_bytes];
    for (index, byte) in value.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(17).wrapping_add(11);
    }
    println!(
        "RSEXT4_BENCH_CONFIG commit={commit} arch={arch} backend={backend} feature={feature} \
         workload=xattr-external value_bytes={value_bytes} warmups={warmups} runs={runs} \
         block_size={BLOCK_SIZE} journal=true"
    );

    for _ in 0..warmups {
        black_box(run_xattr_once(&value));
    }

    let mut samples = Vec::with_capacity(runs);
    for run in 0..runs {
        let sample = run_xattr_once(&value);
        println!(
            "RSEXT4_BENCH_RESULT commit={commit} arch={arch} backend={backend} feature={feature} \
             workload=xattr-external run={run} set_sync_ns={} get_ns={} remove_sync_ns={}",
            sample.set_sync.as_nanos(),
            sample.get.as_nanos(),
            sample.remove_sync.as_nanos()
        );
        samples.push(sample);
    }

    let set_sync = samples
        .iter()
        .map(|sample| sample.set_sync.as_nanos())
        .collect::<Vec<_>>();
    let get = samples
        .iter()
        .map(|sample| sample.get.as_nanos())
        .collect::<Vec<_>>();
    let remove_sync = samples
        .iter()
        .map(|sample| sample.remove_sync.as_nanos())
        .collect::<Vec<_>>();
    println!(
        "RSEXT4_BENCH_SUMMARY commit={commit} arch={arch} backend={backend} feature={feature} \
         workload=xattr-external set_sync_median_ns={} set_sync_p95_ns={} get_median_ns={} \
         get_p95_ns={} remove_sync_median_ns={} remove_sync_p95_ns={}",
        percentile(&set_sync, 1, 2),
        percentile(&set_sync, 95, 100),
        percentile(&get, 1, 2),
        percentile(&get, 95, 100),
        percentile(&remove_sync, 1, 2),
        percentile(&remove_sync, 95, 100),
    );
}

fn run_sync_cycle_benchmark(config: BenchmarkConfig<'_>, payload: &[u8]) {
    let BenchmarkConfig {
        commit,
        arch,
        backend,
        feature,
        warmups,
        runs,
    } = config;
    println!(
        "RSEXT4_BENCH_CONFIG commit={commit} arch={arch} backend={backend} feature={feature} \
         workload=sync-cycle bytes={} warmups={warmups} runs={runs} block_size={BLOCK_SIZE} \
         journal=true",
        payload.len()
    );

    for _ in 0..warmups {
        black_box(run_sync_cycle_once(payload));
    }

    let mut samples = Vec::with_capacity(runs);
    for run in 0..runs {
        let sample = run_sync_cycle_once(payload);
        println!(
            "RSEXT4_BENCH_RESULT commit={commit} arch={arch} backend={backend} feature={feature} \
             workload=sync-cycle run={run} dirty_sync_ns={} clean_sync_ns={} unmount_ns={}",
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
        "RSEXT4_BENCH_SUMMARY commit={commit} arch={arch} backend={backend} feature={feature} \
         workload=sync-cycle dirty_sync_median_ns={} dirty_sync_p95_ns={} clean_sync_median_ns={} \
         clean_sync_p95_ns={} unmount_median_ns={} unmount_p95_ns={}",
        percentile(&dirty_sync, 1, 2),
        percentile(&dirty_sync, 95, 100),
        percentile(&clean_sync, 1, 2),
        percentile(&clean_sync, 95, 100),
        percentile(&unmount, 1, 2),
        percentile(&unmount, 95, 100),
    );
}

fn run_htree_readdir_benchmark(config: BenchmarkConfig<'_>, entries: usize, batch_entries: usize) {
    let BenchmarkConfig {
        commit,
        arch,
        backend,
        feature,
        warmups,
        runs,
    } = config;
    println!(
        "RSEXT4_BENCH_CONFIG commit={commit} arch={arch} backend={backend} feature={feature} \
         workload=htree-readdir entries={entries} batch_entries={batch_entries} warmups={warmups} \
         runs={runs} block_size={BLOCK_SIZE} journal=true"
    );

    for _ in 0..warmups {
        black_box(run_htree_readdir_once(entries, batch_entries));
    }

    let mut samples = Vec::with_capacity(runs);
    for run in 0..runs {
        let sample = run_htree_readdir_once(entries, batch_entries);
        println!(
            "RSEXT4_BENCH_RESULT commit={commit} arch={arch} backend={backend} feature={feature} \
             workload=htree-readdir run={run} readdir_ns={} entries={} calls={}",
            sample.readdir.as_nanos(),
            sample.entries,
            sample.calls
        );
        samples.push(sample);
    }

    let readdir = samples
        .iter()
        .map(|sample| sample.readdir.as_nanos())
        .collect::<Vec<_>>();
    let calls = samples
        .iter()
        .map(|sample| sample.calls as u128)
        .collect::<Vec<_>>();
    println!(
        "RSEXT4_BENCH_SUMMARY commit={commit} arch={arch} backend={backend} feature={feature} \
         workload=htree-readdir readdir_median_ns={} readdir_p95_ns={} calls_median={} \
         calls_p95={}",
        percentile(&readdir, 1, 2),
        percentile(&readdir, 95, 100),
        percentile(&calls, 1, 2),
        percentile(&calls, 95, 100),
    );
}

fn main() {
    let bytes = env_usize("RSEXT4_BENCH_BYTES", DEFAULT_BYTES);
    let warmups = env_usize("RSEXT4_BENCH_WARMUPS", DEFAULT_WARMUPS);
    let runs = env_usize("RSEXT4_BENCH_RUNS", DEFAULT_RUNS);
    let commit = marker_value("RSEXT4_BENCH_COMMIT", "unknown");
    let arch = marker_value("RSEXT4_BENCH_ARCH", env::consts::ARCH);
    let backend = marker_value("RSEXT4_BENCH_BACKEND", "memory");
    let feature = marker_value("RSEXT4_BENCH_FEATURE", "metadata_csum+64bit+journal");
    let workload = marker_value("RSEXT4_BENCH_WORKLOAD", "sequential");
    assert!(bytes > 0 && bytes.is_multiple_of(BLOCK_SIZE));
    assert!(runs > 0);
    let config = BenchmarkConfig {
        commit: &commit,
        arch: &arch,
        backend: &backend,
        feature: &feature,
        warmups,
        runs,
    };

    if workload == "xattr-external" {
        let value_bytes = env_usize("RSEXT4_BENCH_XATTR_BYTES", DEFAULT_XATTR_BYTES);
        assert!(
            value_bytes > 256,
            "external xattr fixture must not fit inline"
        );
        run_xattr_benchmark(config, value_bytes);
        return;
    }
    if workload == "htree-readdir" {
        let entries = env_usize("RSEXT4_BENCH_HTREE_ENTRIES", DEFAULT_HTREE_ENTRIES);
        let batch_entries = env_usize("RSEXT4_BENCH_BATCH_ENTRIES", DEFAULT_HTREE_BATCH_ENTRIES);
        assert!(entries > 0 && batch_entries > 0);
        run_htree_readdir_benchmark(config, entries, batch_entries);
        return;
    }
    let mut payload = vec![0u8; bytes];
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(31).wrapping_add(7);
    }

    if workload == "sync-cycle" {
        run_sync_cycle_benchmark(config, &payload);
        return;
    }
    assert_eq!(workload, "sequential", "unsupported benchmark workload");

    println!(
        "RSEXT4_BENCH_CONFIG commit={commit} arch={arch} backend={backend} feature={feature} \
         workload=sequential bytes={bytes} warmups={warmups} runs={runs} block_size={BLOCK_SIZE} \
         journal=true"
    );

    for _ in 0..warmups {
        black_box(run_once(&payload));
    }

    let mut samples = Vec::with_capacity(runs);
    for run in 0..runs {
        let sample = run_once(&payload);
        println!(
            "RSEXT4_BENCH_RESULT commit={commit} arch={arch} backend={backend} feature={feature} \
             workload=sequential run={run} write_ns={} read_ns={} sync_ns={}",
            sample.write.as_nanos(),
            sample.read.as_nanos(),
            sample.sync.as_nanos()
        );
        samples.push(sample);
    }

    let write = samples
        .iter()
        .map(|sample| sample.write.as_nanos())
        .collect::<Vec<_>>();
    let read = samples
        .iter()
        .map(|sample| sample.read.as_nanos())
        .collect::<Vec<_>>();
    let sync = samples
        .iter()
        .map(|sample| sample.sync.as_nanos())
        .collect::<Vec<_>>();

    println!(
        "RSEXT4_BENCH_SUMMARY commit={commit} arch={arch} backend={backend} feature={feature} \
         workload=sequential write_median_ns={} write_p95_ns={} read_median_ns={} read_p95_ns={} \
         sync_median_ns={} sync_p95_ns={}",
        percentile(&write, 1, 2),
        percentile(&write, 95, 100),
        percentile(&read, 1, 2),
        percentile(&read, 95, 100),
        percentile(&sync, 1, 2),
        percentile(&sync, 95, 100),
    );
}
