use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const BENCH_DIR: &str = "/root/block-rw-bench";
const TOTAL_BYTES: usize = 64 * 1024 * 1024;
const BLOCK_SIZES: [usize; 3] = [4 * 1024, 64 * 1024, 1024 * 1024];
const DROP_CACHES_ENV: &str = "BLOCK_RW_BENCH_DROP_CACHES";

fn main() {
    if let Err(err) = run() {
        eprintln!("block-rw-bench: error: {err}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let dir = Path::new(BENCH_DIR);
    fs::create_dir_all(dir)?;

    for &block_size in &BLOCK_SIZES {
        run_case(dir, block_size, TOTAL_BYTES)?;
    }

    println!(
        "block-rw-bench: done cases={} status=ok",
        BLOCK_SIZES.len()
    );
    Ok(())
}

fn run_case(dir: &Path, block_size: usize, bytes: usize) -> io::Result<()> {
    maybe_drop_caches()?;

    let path = case_path(dir, block_size);
    let mut pattern = vec![0; block_size];
    let write_start = Instant::now();
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)?;

    let mut offset = 0usize;
    while offset < bytes {
        let chunk_len = (bytes - offset).min(block_size);
        fill_pattern(&mut pattern[..chunk_len], block_size, offset);
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
    verify_file(&path, block_size, bytes)?;
    let read_elapsed = read_start.elapsed();

    println!(
        "block-rw-bench: case block_size={} bytes={} write_mib_s={:.2} read_mib_s={:.2} fsync_ms={} verify=ok",
        block_size,
        bytes,
        throughput_mib_s(bytes, write_elapsed),
        throughput_mib_s(bytes, read_elapsed),
        duration_ms(fsync_elapsed)
    );

    fs::remove_file(path)?;
    Ok(())
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

fn fill_pattern(buf: &mut [u8], block_size: usize, base_offset: usize) {
    let seed = block_size as u64 ^ 0x5d51_d1f5_a5a5_1234;
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

fn case_path(dir: &Path, block_size: usize) -> PathBuf {
    dir.join(format!("case-{}.bin", block_size))
}

fn maybe_drop_caches() -> io::Result<()> {
    if env::var_os(DROP_CACHES_ENV).is_none() {
        return Ok(());
    }

    fs::write("/proc/sys/vm/drop_caches", b"3\n")
}
