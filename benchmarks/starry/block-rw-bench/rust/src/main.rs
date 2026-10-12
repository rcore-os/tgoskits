use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

use diskstats::DiskstatsProbe;

mod diskstats;

const BENCH_DIR: &str = "/root/block-rw-bench";
const SEQUENTIAL_BYTES: usize = 8 * 1024 * 1024;
const MULTITASK_BYTES_PER_WORKER: usize = 2 * 1024 * 1024;
const MULTITASK_WORKERS: usize = 8;
const ADMA2_MAX_TRANSFER_BYTES: usize = 1_048_064;
const DROP_CACHES_ENV: &str = "BLOCK_RW_BENCH_DROP_CACHES";
const ROOT_DEVICE_ENV: &str = "BLOCK_RW_BENCH_ROOT_DEVICE";
const CONTROLLER_ENV: &str = "BLOCK_RW_BENCH_CONTROLLER";
const SEQUENTIAL_BYTES_ENV: &str = "BLOCK_RW_BENCH_SEQUENTIAL_BYTES";
const MULTITASK_BYTES_ENV: &str = "BLOCK_RW_BENCH_MULTITASK_BYTES_PER_WORKER";
const MULTITASK_WORKERS_ENV: &str = "BLOCK_RW_BENCH_MULTITASK_WORKERS";
const FSYNC_ENV: &str = "BLOCK_RW_BENCH_FSYNC";
const CHECKSUM_ENV: &str = "BLOCK_RW_BENCH_CHECKSUM_SCENARIO";
const SUCCESS_MARKER_ENV: &str = "BLOCK_RW_BENCH_SUCCESS_MARKER";
const MAX_TRANSFER_BYTES_ENV: &str = "BLOCK_RW_BENCH_MAX_TRANSFER_BYTES";
const REQUEST_BYTES_ENV: &str = "BLOCK_RW_BENCH_REQUEST_BYTES";
const SKIP_BOUNDARY_CASES_ENV: &str = "BLOCK_RW_BENCH_SKIP_BOUNDARY_CASES";
const WORKDIR_ENV: &str = "BLOCK_RW_BENCH_WORKDIR";

#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    io_size: usize,
}

struct BenchConfig {
    workdir: PathBuf,
    root_device: String,
    controller: String,
    sequential_bytes: usize,
    multitask_bytes_per_worker: usize,
    multitask_workers: usize,
    fsync: bool,
    checksum: String,
    success_marker: String,
    max_transfer_bytes: usize,
    /// Bytes for one write/read syscall. `None` keeps the historical behavior
    /// where a logical case moves `case.io_size` bytes per syscall.
    request_bytes: Option<usize>,
    /// Drops the hardware-max and planner-split sequential cases while keeping
    /// the sector, page, and multi-task cases.
    skip_boundary_cases: bool,
}

impl BenchConfig {
    fn from_env() -> io::Result<Self> {
        let config = Self {
            workdir: env_path(WORKDIR_ENV, BENCH_DIR),
            root_device: env_string(ROOT_DEVICE_ENV, "/dev/mmcblk0"),
            controller: env_string(CONTROLLER_ENV, "rk3588-dwcmshc-emmc"),
            sequential_bytes: env_usize(SEQUENTIAL_BYTES_ENV, SEQUENTIAL_BYTES)?,
            multitask_bytes_per_worker: env_usize(MULTITASK_BYTES_ENV, MULTITASK_BYTES_PER_WORKER)?,
            multitask_workers: env_usize(MULTITASK_WORKERS_ENV, MULTITASK_WORKERS)?,
            fsync: env_bool(FSYNC_ENV, true)?,
            checksum: env_string(CHECKSUM_ENV, "pattern"),
            success_marker: env_string(SUCCESS_MARKER_ENV, "BLOCK_RW_BENCH_PASSED"),
            max_transfer_bytes: env_usize(MAX_TRANSFER_BYTES_ENV, ADMA2_MAX_TRANSFER_BYTES)?,
            request_bytes: env_optional_usize(REQUEST_BYTES_ENV)?,
            skip_boundary_cases: env_bool(SKIP_BOUNDARY_CASES_ENV, false)?,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> io::Result<()> {
        if self.root_device.is_empty()
            || self.controller.is_empty()
            || self.success_marker.is_empty()
            || !self.workdir.is_absolute()
            || self.sequential_bytes == 0
            || self.multitask_bytes_per_worker == 0
            || self.multitask_workers == 0
            || self.max_transfer_bytes < 512
            || !self.max_transfer_bytes.is_multiple_of(512)
        {
            return Err(io::Error::other(
                "block benchmark sizes, workers, device, controller, and marker must be valid",
            ));
        }
        if let Some(request_bytes) = self.request_bytes
            && (request_bytes < 512 || !request_bytes.is_multiple_of(512))
        {
            return Err(io::Error::other(format!(
                "{REQUEST_BYTES_ENV} must be a nonzero multiple of 512 bytes"
            )));
        }
        if self.checksum != "pattern" {
            return Err(io::Error::other(format!(
                "{CHECKSUM_ENV} only supports the deterministic `pattern` verifier"
            )));
        }
        Ok(())
    }

    /// Bytes for one write/read syscall of `case`.
    ///
    /// A logical case is never split across two logical units, so a configured
    /// request size larger than the case size degrades to the case size.
    fn request_bytes_for(&self, case: Case) -> usize {
        self.request_bytes.map_or(case.io_size, |request_bytes| {
            request_bytes.min(case.io_size)
        })
    }

    /// Ordered sequential cases for this run.
    ///
    /// The hardware-max and planner-split boundary cases are always the last
    /// two entries, and they are omitted entirely when the board switch drops
    /// them, so the remaining `sector` and `page` cases keep their positions.
    fn sequential_cases(&self) -> io::Result<Vec<Case>> {
        let mut cases = vec![Case::new("sector", 512), Case::new("page", 4 * 1024)];
        if self.skip_boundary_cases {
            return Ok(cases);
        }
        let planner_split_bytes = self
            .max_transfer_bytes
            .checked_add(512)
            .ok_or_else(|| io::Error::other("maximum transfer size overflows planner split"))?;
        cases.push(Case::new("hardware-max", self.max_transfer_bytes));
        cases.push(Case::new("planner-split", planner_split_bytes));
        Ok(cases)
    }
}

fn env_string(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.into())
}

fn env_path(name: &str, default: &str) -> PathBuf {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

fn env_usize(name: &str, default: usize) -> io::Result<usize> {
    env::var(name).map_or(Ok(default), |value| {
        value
            .parse()
            .map_err(|_| io::Error::other(format!("{name} must be an integer")))
    })
}

fn env_optional_usize(name: &str) -> io::Result<Option<usize>> {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .map_or(Ok(None), |value| {
            value
                .parse()
                .map(Some)
                .map_err(|_| io::Error::other(format!("{name} must be an integer")))
        })
}

fn env_bool(name: &str, default: bool) -> io::Result<bool> {
    env::var(name).map_or(Ok(default), |value| match value.as_str() {
        "1" | "true" | "yes" => Ok(true),
        "0" | "false" | "no" => Ok(false),
        _ => Err(io::Error::other(format!("{name} must be true or false"))),
    })
}

impl Case {
    const fn new(name: &'static str, io_size: usize) -> Self {
        Self { name, io_size }
    }
}

fn main() {
    if let Err(err) = run() {
        eprintln!("block-rw-bench: error: {err}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let config = BenchConfig::from_env()?;
    let dir = config.workdir.as_path();
    fs::create_dir_all(dir)?;
    let diskstats = verify_root_device(&config)?;
    println!(
        "block-rw-bench: io_model=buffered-file write_scope=write-syscalls fsync={} \
         drop_caches={} boundary=full-file-stack-not-raw-request-latency",
        config.fsync,
        env::var_os(DROP_CACHES_ENV).is_some()
    );
    let sequential_cases = config.sequential_cases()?;
    if config.skip_boundary_cases {
        println!(
            "block-rw-bench: skip boundary_cases=hardware-max,planner-split \
             boundary_coverage=partial switch={SKIP_BOUNDARY_CASES_ENV}"
        );
        io::stdout().flush()?;
    }

    for case in &sequential_cases {
        println!(
            "block-rw-bench: start case={} io_size={} bytes={}",
            case.name, case.io_size, config.sequential_bytes
        );
        io::stdout().flush()?;
        run_case(
            dir,
            *case,
            config.sequential_bytes,
            config.request_bytes_for(*case),
            config.fsync,
            &diskstats,
        )?;
    }
    println!(
        "block-rw-bench: start case=multitask tasks={} io_size={} bytes_per_task={}",
        config.multitask_workers,
        4 * 1024,
        config.multitask_bytes_per_worker
    );
    io::stdout().flush()?;
    run_multitask_case(dir, &config, &diskstats)?;

    println!(
        "block-rw-bench: done cases={} status=ok",
        sequential_cases.len() + 1
    );
    println!("{}", config.success_marker);
    io::stdout().flush()?;
    Ok(())
}

fn verify_root_device(config: &BenchConfig) -> io::Result<DiskstatsProbe> {
    let mounts = fs::read_to_string("/proc/mounts")?;
    let root_source = mounts
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let source = fields.next()?;
            (fields.next()? == "/").then_some(source)
        })
        .ok_or_else(|| io::Error::other("root mount is absent from /proc/mounts"))?;
    if !root_device_matches(root_source, &config.root_device) {
        return Err(io::Error::other(format!(
            "root-device mismatch: expected {} or one of its partitions, found {root_source}",
            config.root_device
        )));
    }
    println!(
        "block-rw-bench: root_device={root_source} controller={} status=ok",
        config.controller
    );
    io::stdout().flush()?;
    Ok(DiskstatsProbe::for_root(root_source, &config.root_device))
}

fn root_device_matches(root_source: &str, expected: &str) -> bool {
    if root_source == expected {
        return true;
    }

    let Some(suffix) = root_source.strip_prefix(expected) else {
        return false;
    };
    let partition = if expected.as_bytes().last().is_some_and(u8::is_ascii_digit) {
        suffix.strip_prefix('p')
    } else {
        Some(suffix)
    };
    partition.is_some_and(|partition| {
        !partition.is_empty() && partition.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// Nearest-rank percentile over per-operation samples in nanoseconds.
fn percentile_ns(samples: &mut [u64], percent: usize) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    let rank = (samples.len() * percent).div_ceil(100);
    samples[rank.max(1) - 1]
}

/// Visit every write or read request of one case in order.
///
/// `bytes` are covered as logical units of at most `logical_size` bytes, and
/// every unit is split into requests of at most `request_size` bytes. Splitting
/// only changes how many bytes one syscall moves: the logical unit boundaries,
/// the pattern, and the total byte count stay defined by `bytes` and
/// `logical_size`, and a unit's final request carries its remaining tail.
fn for_each_request(
    bytes: usize,
    logical_size: usize,
    request_size: usize,
    mut request: impl FnMut(usize, usize) -> io::Result<()>,
) -> io::Result<()> {
    if logical_size == 0 || request_size == 0 {
        return Err(io::Error::other(
            "logical case size and request size must be nonzero",
        ));
    }

    let mut offset = 0usize;
    while offset < bytes {
        let unit_len = (bytes - offset).min(logical_size);
        let mut unit_offset = 0usize;
        while unit_offset < unit_len {
            let request_len = (unit_len - unit_offset).min(request_size);
            request(offset + unit_offset, request_len)?;
            unit_offset += request_len;
        }
        offset += unit_len;
    }
    Ok(())
}

fn run_case(
    dir: &Path,
    case: Case,
    bytes: usize,
    request_bytes: usize,
    fsync: bool,
    diskstats: &DiskstatsProbe,
) -> io::Result<()> {
    maybe_drop_caches()?;

    let path = case_path(dir, case.name);
    // The pattern seed and the logical case size stay tied to `case.io_size`;
    // `request_bytes` only caps how many bytes one syscall moves.
    let mut pattern = vec![0; case.io_size];
    let before_write = diskstats.snapshot()?;
    let write_start = Instant::now();
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)?;

    let mut write_latencies = Vec::new();
    for_each_request(bytes, case.io_size, request_bytes, |offset, request_len| {
        fill_pattern(&mut pattern[..request_len], case.io_size, offset);
        let op_start = Instant::now();
        file.write_all(&pattern[..request_len])?;
        write_latencies.push(op_start.elapsed().as_nanos() as u64);
        Ok(())
    })?;
    let write_elapsed = write_start.elapsed();
    let after_write = diskstats.snapshot()?;

    let fsync_start = Instant::now();
    if fsync {
        file.sync_all()?;
    }
    let fsync_elapsed = fsync_start.elapsed();
    let after_fsync = diskstats.snapshot()?;
    drop(file);

    maybe_drop_caches()?;

    let before_read = diskstats.snapshot()?;
    let read_start = Instant::now();
    let mut read_latencies = Vec::new();
    verify_file(
        &path,
        case.io_size,
        request_bytes,
        bytes,
        &mut read_latencies,
    )?;
    let read_elapsed = read_start.elapsed();
    let after_read = diskstats.snapshot()?;

    println!(
        "block-rw-bench: case={} io_size={} bytes={} write_mib_s={:.2} read_mib_s={:.2} \
         fsync_ms={} verify=ok",
        case.name,
        case.io_size,
        bytes,
        throughput_mib_s(bytes, write_elapsed),
        throughput_mib_s(bytes, read_elapsed),
        duration_ms(fsync_elapsed)
    );
    println!(
        "block-rw-bench: result case={} io_size={} bytes={} operations={} write_bytes_per_sec={} \
         read_bytes_per_sec={} write_p50_ns={} write_p95_ns={} write_p99_ns={} read_p50_ns={} \
         read_p95_ns={} read_p99_ns={}",
        case.name,
        case.io_size,
        bytes,
        write_latencies.len(),
        throughput_bytes_per_sec(bytes, write_elapsed),
        throughput_bytes_per_sec(bytes, read_elapsed),
        percentile_ns(&mut write_latencies, 50),
        percentile_ns(&mut write_latencies, 95),
        percentile_ns(&mut write_latencies, 99),
        percentile_ns(&mut read_latencies, 50),
        percentile_ns(&mut read_latencies, 95),
        percentile_ns(&mut read_latencies, 99),
    );
    diskstats.print_delta(case.name, "write", before_write, after_write);
    diskstats.print_delta(case.name, "fsync", after_write, after_fsync);
    diskstats.print_delta(case.name, "read", before_read, after_read);
    io::stdout().flush()?;

    fs::remove_file(path)?;
    Ok(())
}

fn run_multitask_case(
    dir: &Path,
    config: &BenchConfig,
    diskstats: &DiskstatsProbe,
) -> io::Result<()> {
    let barrier = Arc::new(Barrier::new(config.multitask_workers));
    let before = diskstats.snapshot()?;
    let started = Instant::now();
    let bytes_per_worker = config.multitask_bytes_per_worker;
    let multitask_case = Case::new("multitask", 4 * 1024);
    let request_bytes = config.request_bytes_for(multitask_case);
    let fsync = config.fsync;
    let mut workers = Vec::with_capacity(config.multitask_workers);
    for worker_id in 0..config.multitask_workers {
        let barrier = Arc::clone(&barrier);
        let dir = dir.to_path_buf();
        workers.push(thread::spawn(move || {
            barrier.wait();
            let name = format!("multitask-{worker_id}");
            let path = dir.join(format!("case-{name}.bin"));
            run_path_case(
                &path,
                multitask_case,
                bytes_per_worker,
                request_bytes,
                worker_id,
                fsync,
            )
        }));
    }

    for worker in workers {
        worker
            .join()
            .map_err(|_| io::Error::other("multitask worker panicked"))??;
    }
    let after = diskstats.snapshot()?;
    println!(
        "block-rw-bench: case=multitask tasks={} io_size={} bytes_per_task={} elapsed_ms={} \
         fsync={} verify=ok",
        config.multitask_workers,
        4 * 1024,
        config.multitask_bytes_per_worker,
        duration_ms(started.elapsed()),
        fsync
    );
    diskstats.print_delta("multitask", "total", before, after);
    io::stdout().flush()?;
    Ok(())
}

fn run_path_case(
    path: &Path,
    case: Case,
    bytes: usize,
    request_bytes: usize,
    worker_id: usize,
    fsync: bool,
) -> io::Result<()> {
    let mut pattern = vec![0; case.io_size];
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)?;
    for_each_request(bytes, case.io_size, request_bytes, |offset, request_len| {
        fill_pattern_for_worker(&mut pattern[..request_len], case.io_size, offset, worker_id);
        file.write_all(&pattern[..request_len])?;
        Ok(())
    })?;
    if fsync {
        file.sync_all()?;
    }
    drop(file);
    verify_worker_file(path, case.io_size, request_bytes, bytes, worker_id)?;
    fs::remove_file(path)
}

fn verify_file(
    path: &Path,
    block_size: usize,
    request_bytes: usize,
    bytes: usize,
    read_latencies: &mut Vec<u64>,
) -> io::Result<()> {
    // Deliberately unbuffered: every read must cross the syscall/filesystem
    // boundary so the recorded latencies cover the full file-I/O path.
    let mut file = File::open(path)?;
    let mut actual = vec![0; block_size];
    let mut expected = vec![0; block_size];

    for_each_request(bytes, block_size, request_bytes, |offset, request_len| {
        let op_start = Instant::now();
        file.read_exact(&mut actual[..request_len])?;
        read_latencies.push(op_start.elapsed().as_nanos() as u64);
        fill_pattern(&mut expected[..request_len], block_size, offset);
        if actual[..request_len] != expected[..request_len] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "verify mismatch block_size={} offset={} expected={:02x} actual={:02x}",
                    block_size, offset, expected[0], actual[0]
                ),
            ));
        }
        Ok(())
    })
}

fn verify_worker_file(
    path: &Path,
    block_size: usize,
    request_bytes: usize,
    bytes: usize,
    worker_id: usize,
) -> io::Result<()> {
    let mut file = File::open(path)?;
    let mut actual = vec![0; block_size];
    let mut expected = vec![0; block_size];

    for_each_request(bytes, block_size, request_bytes, |offset, request_len| {
        file.read_exact(&mut actual[..request_len])?;
        fill_pattern_for_worker(&mut expected[..request_len], block_size, offset, worker_id);
        if actual[..request_len] != expected[..request_len] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "multitask verify mismatch worker={worker_id} offset={offset} expected={:02x} \
                     actual={:02x}",
                    expected[0], actual[0]
                ),
            ));
        }
        Ok(())
    })
}

fn fill_pattern(buf: &mut [u8], block_size: usize, base_offset: usize) {
    fill_pattern_for_worker(buf, block_size, base_offset, 0);
}

fn fill_pattern_for_worker(
    buf: &mut [u8],
    block_size: usize,
    base_offset: usize,
    worker_id: usize,
) {
    let seed = block_size as u64 ^ (worker_id as u64).rotate_left(17) ^ 0x5d51_d1f5_a5a5_1234;
    for (index, byte) in buf.iter_mut().enumerate() {
        let pos = (base_offset + index) as u64;
        *byte = pos
            .wrapping_mul(1103515245)
            .wrapping_add(seed)
            .rotate_left((pos & 7) as u32) as u8;
    }
}

fn throughput_bytes_per_sec(bytes: usize, elapsed: Duration) -> u64 {
    let elapsed_ns = elapsed.as_nanos().max(1) as u64;
    (bytes as u64).saturating_mul(1_000_000_000) / elapsed_ns
}

fn throughput_mib_s(bytes: usize, elapsed: Duration) -> f64 {
    let seconds = elapsed.as_secs_f64().max(0.000_001);
    bytes as f64 / (1024.0 * 1024.0) / seconds
}

fn duration_ms(elapsed: Duration) -> u128 {
    elapsed.as_millis()
}

fn case_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("case-{name}.bin"))
}

fn maybe_drop_caches() -> io::Result<()> {
    if env::var_os(DROP_CACHES_ENV).is_none() {
        return Ok(());
    }

    fs::write("/proc/sys/vm/drop_caches", b"3\n")
}

#[cfg(test)]
mod tests {
    use super::{BenchConfig, Case, for_each_request, root_device_matches};

    fn valid_config() -> BenchConfig {
        BenchConfig {
            workdir: "/root/block-rw-bench".into(),
            root_device: "/dev/mmcblk0".into(),
            controller: "test-controller".into(),
            sequential_bytes: 512,
            multitask_bytes_per_worker: 512,
            multitask_workers: 1,
            fsync: false,
            checksum: "pattern".into(),
            success_marker: "TEST_PASSED".into(),
            max_transfer_bytes: 512,
            request_bytes: None,
            skip_boundary_cases: false,
        }
    }

    fn case_names(cases: &[Case]) -> Vec<&'static str> {
        cases.iter().map(|case| case.name).collect()
    }

    #[test]
    fn bench_config_rejects_relative_workdir() {
        let config = BenchConfig {
            workdir: "relative".into(),
            ..valid_config()
        };

        assert!(config.validate().is_err());
    }

    #[test]
    fn bench_config_requires_512_aligned_request_bytes() {
        for request_bytes in [0, 1, 511, 4097] {
            let config = BenchConfig {
                request_bytes: Some(request_bytes),
                ..valid_config()
            };
            assert!(
                config.validate().is_err(),
                "request_bytes={request_bytes} must be rejected"
            );
        }

        for request_bytes in [512, 4096, 4608] {
            let config = BenchConfig {
                request_bytes: Some(request_bytes),
                ..valid_config()
            };
            assert!(
                config.validate().is_ok(),
                "request_bytes={request_bytes} must be accepted"
            );
        }
    }

    #[test]
    fn request_bytes_never_exceed_the_logical_case_size() {
        let case = Case::new("planner-split", 4608);

        assert_eq!(valid_config().request_bytes_for(case), 4608);

        let config = BenchConfig {
            request_bytes: Some(4096),
            ..valid_config()
        };
        assert_eq!(config.request_bytes_for(case), 4096);

        let config = BenchConfig {
            request_bytes: Some(8192),
            ..valid_config()
        };
        assert_eq!(config.request_bytes_for(case), 4608);
    }

    #[test]
    fn request_split_preserves_logical_units_and_tail_bytes() {
        let mut requests = Vec::new();
        for_each_request(2 * 4608 + 100, 4608, 4096, |offset, request_len| {
            requests.push((offset, request_len));
            Ok(())
        })
        .expect("request split must succeed");

        assert_eq!(
            requests,
            [
                (0, 4096),
                (4096, 512),
                (4608, 4096),
                (8704, 512),
                (9216, 100)
            ]
        );
    }

    #[test]
    fn request_split_rejects_zero_sizes() {
        assert!(for_each_request(512, 512, 0, |_, _| Ok(())).is_err());
        assert!(for_each_request(512, 0, 512, |_, _| Ok(())).is_err());
        assert!(for_each_request(0, 512, 512, |_, _| Ok(())).is_ok());
    }

    #[test]
    fn skip_boundary_cases_keeps_only_the_sector_and_page_cases() {
        assert_eq!(
            case_names(
                &valid_config()
                    .sequential_cases()
                    .expect("the default run must build the full case list")
            ),
            ["sector", "page", "hardware-max", "planner-split"]
        );

        let trimmed = BenchConfig {
            skip_boundary_cases: true,
            ..valid_config()
        };
        assert_eq!(
            case_names(
                &trimmed
                    .sequential_cases()
                    .expect("skipping boundary cases must still build a case list")
            ),
            ["sector", "page"]
        );
    }

    #[test]
    fn root_device_match_accepts_only_the_device_or_numeric_partition() {
        assert!(root_device_matches("/dev/mmcblk0", "/dev/mmcblk0"));
        assert!(root_device_matches("/dev/mmcblk0p2", "/dev/mmcblk0"));
        assert!(!root_device_matches("/dev/mmcblk00", "/dev/mmcblk0"));
        assert!(!root_device_matches("/dev/mmcblk0p", "/dev/mmcblk0"));
        assert!(!root_device_matches("/dev/mmcblk0p2x", "/dev/mmcblk0"));
        assert!(!root_device_matches("/dev/nvme0n1p2", "/dev/mmcblk0"));
    }

    #[test]
    fn root_device_match_accepts_only_numeric_scsi_partition_suffixes() {
        assert!(root_device_matches("/dev/sda", "/dev/sda"));
        assert!(root_device_matches("/dev/sda1", "/dev/sda"));
        assert!(root_device_matches("/dev/sda12", "/dev/sda"));
        assert!(!root_device_matches("/dev/sdaa", "/dev/sda"));
        assert!(!root_device_matches("/dev/sda1x", "/dev/sda"));
        assert!(!root_device_matches("/dev/sdap1", "/dev/sda"));
        assert!(!root_device_matches("/dev/sdb1", "/dev/sda"));
    }
}
