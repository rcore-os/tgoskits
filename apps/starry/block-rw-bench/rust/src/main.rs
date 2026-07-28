use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

const BENCH_DIR: &str = "/root/block-rw-bench";
const SEQUENTIAL_BYTES: usize = 8 * 1024 * 1024;
const MULTITASK_BYTES_PER_WORKER: usize = 2 * 1024 * 1024;
const MULTITASK_WORKERS: usize = 8;
const ADMA2_MAX_TRANSFER_BYTES: usize = 1_048_448;
const SEQUENTIAL_CASES: [Case; 4] = [
    Case::new("sector", 512),
    Case::new("page", 4 * 1024),
    Case::new("adma-max", ADMA2_MAX_TRANSFER_BYTES),
    Case::new("adma-split", ADMA2_MAX_TRANSFER_BYTES + 512),
];
const DROP_CACHES_ENV: &str = "BLOCK_RW_BENCH_DROP_CACHES";

#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    io_size: usize,
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
    let dir = Path::new(BENCH_DIR);
    fs::create_dir_all(dir)?;
    verify_root_device()?;

    for case in SEQUENTIAL_CASES {
        println!(
            "block-rw-bench: start case={} io_size={} bytes={}",
            case.name, case.io_size, SEQUENTIAL_BYTES
        );
        io::stdout().flush()?;
        run_case(dir, case, SEQUENTIAL_BYTES)?;
    }
    println!(
        "block-rw-bench: start case=multitask tasks={} io_size={} bytes_per_task={}",
        MULTITASK_WORKERS,
        4 * 1024,
        MULTITASK_BYTES_PER_WORKER
    );
    io::stdout().flush()?;
    run_multitask_case(dir)?;

    println!(
        "block-rw-bench: done cases={} status=ok",
        SEQUENTIAL_CASES.len() + 1
    );
    println!("ORANGEPI_BLOCK_RW_BENCH_PASSED");
    io::stdout().flush()?;
    Ok(())
}

fn verify_root_device() -> io::Result<()> {
    let mounts = fs::read_to_string("/proc/mounts")?;
    let root_source = mounts
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let source = fields.next()?;
            (fields.next()? == "/").then_some(source)
        })
        .ok_or_else(|| io::Error::other("root mount is absent from /proc/mounts"))?;
    let root_is_emmc = root_source == "/dev/mmcblk0"
        || root_source
            .strip_prefix("/dev/mmcblk0p")
            .is_some_and(|partition| {
                !partition.is_empty() && partition.bytes().all(|byte| byte.is_ascii_digit())
            });
    if !root_is_emmc {
        return Err(io::Error::other(format!(
            "root-device mismatch: expected /dev/mmcblk0 or one of its partitions, found \
             {root_source}"
        )));
    }
    println!("block-rw-bench: root_device={root_source} controller=rk3588-dwcmshc-emmc status=ok");
    io::stdout().flush()?;
    Ok(())
}

fn run_case(dir: &Path, case: Case, bytes: usize) -> io::Result<()> {
    maybe_drop_caches()?;

    let path = case_path(dir, case.name);
    let mut pattern = vec![0; case.io_size];
    let write_start = Instant::now();
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)?;

    let mut offset = 0usize;
    while offset < bytes {
        let chunk_len = (bytes - offset).min(case.io_size);
        fill_pattern(&mut pattern[..chunk_len], case.io_size, offset);
        file.write_all(&pattern[..chunk_len])?;
        offset += chunk_len;
    }
    let write_elapsed = write_start.elapsed();

    let fsync_start = Instant::now();
    file.sync_all()?;
    let fsync_elapsed = fsync_start.elapsed();
    drop(file);

    maybe_drop_caches()?;

    let read_start = Instant::now();
    verify_file(&path, case.io_size, bytes)?;
    let read_elapsed = read_start.elapsed();

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
    io::stdout().flush()?;

    fs::remove_file(path)?;
    Ok(())
}

fn run_multitask_case(dir: &Path) -> io::Result<()> {
    let barrier = Arc::new(Barrier::new(MULTITASK_WORKERS));
    let started = Instant::now();
    let mut workers = Vec::with_capacity(MULTITASK_WORKERS);
    for worker_id in 0..MULTITASK_WORKERS {
        let barrier = Arc::clone(&barrier);
        let dir = dir.to_path_buf();
        workers.push(thread::spawn(move || {
            barrier.wait();
            let name = format!("multitask-{worker_id}");
            let path = dir.join(format!("case-{name}.bin"));
            run_path_case(
                &path,
                Case::new("multitask", 4 * 1024),
                MULTITASK_BYTES_PER_WORKER,
                worker_id,
            )
        }));
    }

    for worker in workers {
        worker
            .join()
            .map_err(|_| io::Error::other("multitask worker panicked"))??;
    }
    println!(
        "block-rw-bench: case=multitask tasks={} io_size={} bytes_per_task={} elapsed_ms={} \
         fsync=each verify=ok",
        MULTITASK_WORKERS,
        4 * 1024,
        MULTITASK_BYTES_PER_WORKER,
        duration_ms(started.elapsed())
    );
    io::stdout().flush()?;
    Ok(())
}

fn run_path_case(path: &Path, case: Case, bytes: usize, worker_id: usize) -> io::Result<()> {
    let mut pattern = vec![0; case.io_size];
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)?;
    let mut offset = 0usize;
    while offset < bytes {
        let chunk_len = (bytes - offset).min(case.io_size);
        fill_pattern_for_worker(&mut pattern[..chunk_len], case.io_size, offset, worker_id);
        file.write_all(&pattern[..chunk_len])?;
        offset += chunk_len;
    }
    file.sync_all()?;
    drop(file);
    verify_worker_file(path, case.io_size, bytes, worker_id)?;
    fs::remove_file(path)
}

fn verify_file(path: &Path, block_size: usize, bytes: usize) -> io::Result<()> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut actual = vec![0; block_size];
    let mut expected = vec![0; block_size];
    let mut offset = 0usize;

    while offset < bytes {
        let chunk_len = (bytes - offset).min(block_size);
        reader.read_exact(&mut actual[..chunk_len])?;
        fill_pattern(&mut expected[..chunk_len], block_size, offset);
        if actual[..chunk_len] != expected[..chunk_len] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "verify mismatch block_size={} offset={} expected={:02x} actual={:02x}",
                    block_size, offset, expected[0], actual[0]
                ),
            ));
        }
        offset += chunk_len;
    }

    Ok(())
}

fn verify_worker_file(
    path: &Path,
    block_size: usize,
    bytes: usize,
    worker_id: usize,
) -> io::Result<()> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut actual = vec![0; block_size];
    let mut expected = vec![0; block_size];
    let mut offset = 0usize;

    while offset < bytes {
        let chunk_len = (bytes - offset).min(block_size);
        reader.read_exact(&mut actual[..chunk_len])?;
        fill_pattern_for_worker(&mut expected[..chunk_len], block_size, offset, worker_id);
        if actual[..chunk_len] != expected[..chunk_len] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "multitask verify mismatch worker={worker_id} offset={offset} expected={:02x} \
                     actual={:02x}",
                    expected[0], actual[0]
                ),
            ));
        }
        offset += chunk_len;
    }
    Ok(())
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
