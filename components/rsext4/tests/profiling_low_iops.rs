//! Low-IOPS device profiling harness for rsext4.
//!
//! Wraps rsext4 with a mock block device that simulates a slow (low-IOPS)
//! storage device, then runs cold-cache scenarios so we can see *where* the
//! raw I/O budget is spent and *why* the filesystem is slow on such devices.
//!
//! Run with:
//!   cargo test --test profiling_low_iops -- --nocapture --ignored
//!
//! The device does NOT actually sleep by default — it counts ops and derives a
//! *modeled* wall time from a per-op latency (read 5 ms, write 10 ms ≈ a slow
//! SD / eMMC grade device). Counting is deterministic and fast. Per-scenario
//! counters are deltas measured on a freshly mounted filesystem (cold FS-level
//! caches) so the numbers reflect real cold-cache cost, not warm-cache hits.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Arc;

use rsext4::{
    bmalloc::AbsoluteBN,
    error::{Ext4Error, Ext4Result},
    *,
};

// ─── modeled slow-device latencies + bandwidth ─────────────────────────────
// Cheap SD card: ~200 random-read IOPS (5 ms), ~100 random-write IOPS (10 ms)
// per 4 KiB op, AND slow sequential bandwidth (~2 MB/s write, ~5 MB/s read).
// Modeled phase time = ops×latency + bytes/bandwidth, so both the IOP count
// and the transfer volume matter — exactly the "low IOPS + slow write speed"
// scenario from the goal.
const READ_LATENCY_US: u64 = 5_000;
const WRITE_LATENCY_US: u64 = 10_000;
const READ_BW_BYTES_PER_S: u64 = 5 * 1024 * 1024;
const WRITE_BW_BYTES_PER_S: u64 = 2 * 1024 * 1024;
const LATENCY_MODE: bool = false; // true => actually sleep (sanity only)

#[derive(Default, Clone, Copy)]
struct BlockStats {
    reads: u64,
    writes: u64,
}

/// Shared, interior-mutable counter store (Arc'd so the harness can snapshot
/// without a ref to the device, which is buried inside `Jbd2Dev`).
///
/// Counting model: a low-IOPS device is **IOP-bound**, not bandwidth-bound —
/// a single read/write *call* that transfers N blocks costs ~1 IOP, not N.
/// So `read_ops`/`write_ops` count device-call invocations, and modeled time
/// is `ops × per-op latency`. Per-block touch counters are kept separately
/// only for cache-thrash analysis.
struct Counters {
    read_ops: Cell<u64>,
    write_ops: Cell<u64>,
    read_bytes: Cell<u64>,
    write_bytes: Cell<u64>,
    per_block: std::cell::RefCell<BTreeMap<u64, BlockStats>>,
}

impl Counters {
    fn new() -> Self {
        Self {
            read_ops: Cell::new(0),
            write_ops: Cell::new(0),
            read_bytes: Cell::new(0),
            write_bytes: Cell::new(0),
            per_block: std::cell::RefCell::new(BTreeMap::new()),
        }
    }
    fn bill_read(&self, first_block: u64, nblocks: usize, bytes: usize) {
        self.read_ops.set(self.read_ops.get() + 1);
        self.read_bytes.set(self.read_bytes.get() + bytes as u64);
        let mut pb = self.per_block.borrow_mut();
        for i in 0..nblocks {
            pb.entry(first_block + i as u64).or_default().reads += 1;
        }
        if LATENCY_MODE {
            std::thread::sleep(std::time::Duration::from_micros(READ_LATENCY_US));
        }
    }
    fn bill_write(&self, first_block: u64, nblocks: usize, bytes: usize) {
        self.write_ops.set(self.write_ops.get() + 1);
        self.write_bytes.set(self.write_bytes.get() + bytes as u64);
        let mut pb = self.per_block.borrow_mut();
        for i in 0..nblocks {
            pb.entry(first_block + i as u64).or_default().writes += 1;
        }
        if LATENCY_MODE {
            std::thread::sleep(std::time::Duration::from_micros(WRITE_LATENCY_US));
        }
    }
    fn snapshot(&self) -> Snapshot {
        let read_ops = self.read_ops.get();
        let write_ops = self.write_ops.get();
        let read_bytes = self.read_bytes.get();
        let write_bytes = self.write_bytes.get();
        let op_us = read_ops * READ_LATENCY_US + write_ops * WRITE_LATENCY_US;
        let bw_us = (read_bytes * 1_000_000 / READ_BW_BYTES_PER_S.max(1))
            + (write_bytes * 1_000_000 / WRITE_BW_BYTES_PER_S.max(1));
        Snapshot {
            read_ops,
            write_ops,
            read_bytes,
            write_bytes,
            modeled_us: op_us + bw_us,
        }
    }
    fn reset(&self) {
        self.read_ops.set(0);
        self.write_ops.set(0);
        self.read_bytes.set(0);
        self.write_bytes.set(0);
        self.per_block.borrow_mut().clear();
    }
    fn top(&self, n: usize, writes: bool) -> Vec<(u64, u64)> {
        let mut v: Vec<(u64, u64)> = self
            .per_block
            .borrow()
            .iter()
            .map(|(k, s)| (*k, if writes { s.writes } else { s.reads }))
            .filter(|(_, c)| *c > 0)
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v.truncate(n);
        v
    }
    /// Distinct blocks touched, min/max block id, and a coarse histogram.
    fn hist(&self) -> (u64, u64, u64, Vec<(String, u64)>) {
        let pb = self.per_block.borrow();
        let distinct = pb.len() as u64;
        let min = pb.keys().copied().min().unwrap_or(0);
        let max = pb.keys().copied().max().unwrap_or(0);
        // Buckets: 0-15 (sb/gdt), 16-527 (group0 meta+inode table), 528-4623 (journal), 4624+
        let buckets = [
            ("blk    0-15  (sb/gdt)", 0u64, 16u64),
            ("blk   16-527 (inode tbl)", 16, 528),
            ("blk  528-4623(journal?)", 528, 4624),
            ("blk 4624+    (data)", 4624, u64::MAX),
        ];
        let h: Vec<(String, u64)> = buckets
            .iter()
            .map(|(name, lo, hi)| {
                let c = pb
                    .range(*lo..*hi)
                    .map(|(_, s)| s.writes + s.reads)
                    .sum();
                ((*name).to_string(), c)
            })
            .collect();
        (distinct, min, max, h)
    }
}

struct ProfilingBlockDevice {
    data: Vec<u8>,
    block_size: u32,
    now: Cell<i64>,
    ctr: Arc<Counters>,
}

impl ProfilingBlockDevice {
    fn new(size: usize, ctr: Arc<Counters>) -> Self {
        Self {
            data: vec![0; size],
            block_size: BLOCK_SIZE as u32,
            now: Cell::new(1_700_000_000),
            ctr,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Snapshot {
    read_ops: u64,
    write_ops: u64,
    read_bytes: u64,
    write_bytes: u64,
    modeled_us: u64,
}

impl Snapshot {
    fn fmt(&self) -> String {
        format!(
            "rOps={:>5} wOps={:>5}  rB={:>9} wB={:>9}  modeled={:>8.3} s",
            self.read_ops,
            self.write_ops,
            self.read_bytes,
            self.write_bytes,
            self.modeled_us as f64 / 1_000_000.0,
        )
    }
}

impl BlockDevice for ProfilingBlockDevice {
    fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
        let bid = block_id.to_u32().map_err(|_| Ext4Error::corrupted())? as u64;
        let start = bid as usize * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                (self.data.len() / self.block_size as usize) as u64,
            ));
        }
        let bs = self.block_size as usize;
        let nblocks = buffer.len().div_ceil(bs).max(1);
        // 1 IOP per call — multi-block transfers are a single request on a
        // real low-IOPS device.
        self.ctr.bill_read(bid, nblocks, buffer.len());
        buffer.copy_from_slice(&self.data[start..end]);
        Ok(())
    }
    fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
        let bid = block_id.to_u32().map_err(|_| Ext4Error::corrupted())? as u64;
        let start = bid as usize * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                (self.data.len() / self.block_size as usize) as u64,
            ));
        }
        let bs = self.block_size as usize;
        let nblocks = buffer.len().div_ceil(bs).max(1);
        self.ctr.bill_write(bid, nblocks, buffer.len());
        self.data[start..end].copy_from_slice(buffer);
        Ok(())
    }
    fn open(&mut self) -> Ext4Result<()> {
        Ok(())
    }
    fn close(&mut self) -> Ext4Result<()> {
        Ok(())
    }
    fn total_blocks(&self) -> u64 {
        (self.data.len() / self.block_size as usize) as u64
    }
    fn block_size(&self) -> u32 {
        self.block_size
    }
    fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

// ─── workload constants ────────────────────────────────────────────────────
const N_FILES: usize = 100; // kept below the ~100-entry htree-spill threshold
const SMALL_FILE_BYTES: usize = 512;
const BIG_FILE_BYTES: usize = 1 << 20; // 1 MiB
const N_DIRS: usize = 50;

fn file_path(i: usize) -> String {
    format!("/files/f_{:04}", i)
}

/// Mount a fresh (cold-cache) Jbd2Dev on the given device.
fn cold_mount(device: ProfilingBlockDevice) -> (Jbd2Dev<ProfilingBlockDevice>, Ext4FileSystem) {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, device, true);
    let fs = mount(&mut dev).expect("mount failed");
    (dev, fs)
}

#[test]
#[ignore = "profiling harness, run explicitly"]
fn profile_low_iops_baseline() {
    eprintln!();
    eprintln!("############################################################################");
    eprintln!("# rsext4 low-IOPS profiling  (read={}us write={}us per 4K op)", READ_LATENCY_US, WRITE_LATENCY_US);
    eprintln!("############################################################################");
    let ctr = Arc::new(Counters::new());

    // ── Scenario A: mkfs (one-time provisioning) ────────────────────────────
    let device = ProfilingBlockDevice::new(100 * 1024 * 1024, ctr.clone());
    let mut dev = Jbd2Dev::initial_jbd2dev(0, device, true);
    ctr.reset();
    mkfs(&mut dev).expect("mkfs");
    let mkfs_s = ctr.snapshot();
    eprintln!("[A mkfs        ] {}", mkfs_s.fmt());
    let (distinct, min, max, h) = ctr.hist();
    eprintln!(
        "    distinct blocks touched={} range=[{}, {}]",
        distinct, min, max
    );
    for (name, c) in &h {
        eprintln!("       {} : {} block-touches", name, c);
    }

    // ── Setup: create the dataset once (also = Scenario C: create + commit) ──
    ctr.reset();
    let mut fs = mount(&mut dev).expect("mount");
    let mount_s = ctr.snapshot();
    eprintln!("[  mount(cold)  ] {}", mount_s.fmt());

    ctr.reset();
    mkdir(&mut dev, &mut fs, "/files").expect("mkdir");
    let payload = vec![0xABu8; SMALL_FILE_BYTES];
    for i in 0..N_FILES {
        mkfile(&mut dev, &mut fs, &file_path(i), Some(&payload), None).expect("mkfile");
    }
    let create_s = ctr.snapshot();
    eprintln!("[C create {:>3} ] {}", N_FILES, create_s.fmt());
    eprintln!(
        "    per-file (pre-commit): reads={:.2} writes={:.2} modeled={:.2} ms",
        create_s.read_ops as f64 / N_FILES as f64,
        create_s.write_ops as f64 / N_FILES as f64,
        create_s.modeled_us as f64 / N_FILES as f64 / 1000.0,
    );

    ctr.reset();
    umount(fs, &mut dev).expect("umount");
    let commit_s = ctr.snapshot();
    eprintln!("[  umount commit] {}", commit_s.fmt());
    eprintln!(
        "    => total create+commit per-file: modeled={:.2} ms",
        (create_s.modeled_us + commit_s.modeled_us) as f64 / N_FILES as f64 / 1000.0
    );

    // Take the device back; all later scenarios mount cold on the same data.
    let device = dev.into_inner();

    // ── Scenario B: cold mount + read 100 files (cold-cache read) ───────────
    ctr.reset();
    let (mut dev, mut fs) = cold_mount(device);
    let mount_b = ctr.snapshot();
    let payload_read = vec![0xABu8; SMALL_FILE_BYTES];
    for i in 0..N_FILES {
        let d = read_file(&mut dev, &mut fs, &file_path(i)).expect("read_file");
        assert_eq!(d, payload_read);
    }
    let read_s = ctr.snapshot();
    eprintln!("[B read  {:>3} ] {}", N_FILES, read_s.fmt());
    eprintln!(
        "    (cold mount cost {} + read cost). per-file read: reads={:.2} writes={:.2} modeled={:.2} ms",
        fmt_us(mount_b.modeled_us),
        (read_s.read_ops - mount_b.read_ops) as f64 / N_FILES as f64,
        (read_s.write_ops - mount_b.write_ops) as f64 / N_FILES as f64,
        (read_s.modeled_us - mount_b.modeled_us) as f64 / N_FILES as f64 / 1000.0,
    );
    umount(fs, &mut dev).expect("umount");
    let device = dev.into_inner();

    // ── Scenario D: cold mount + append 1 MiB in 4 KiB chunks + commit ──────
    ctr.reset();
    let (mut dev, mut fs) = cold_mount(device);
    mkfile(&mut dev, &mut fs, "/big.bin", None, None).expect("mkfile big");
    ctr.reset(); // reset so the cost is the append itself
    let chunk = vec![0xCDu8; BLOCK_SIZE];
    let n_chunks = BIG_FILE_BYTES / BLOCK_SIZE;
    let mut off = 0u64;
    for _ in 0..n_chunks {
        write_file(&mut dev, &mut fs, "/big.bin", off, &chunk).expect("write_file");
        off += BLOCK_SIZE as u64;
    }
    let bw_s = ctr.snapshot();
    eprintln!("[D write 1MiB  ] {}", bw_s.fmt());
    eprintln!(
        "    per-chunk: reads={:.2} writes={:.2} modeled={:.2} ms",
        bw_s.read_ops as f64 / n_chunks as f64,
        bw_s.write_ops as f64 / n_chunks as f64,
        bw_s.modeled_us as f64 / n_chunks as f64 / 1000.0,
    );
    ctr.reset();
    umount(fs, &mut dev).expect("umount");
    let bigcommit_s = ctr.snapshot();
    eprintln!("[  umount commit] {}", bigcommit_s.fmt());
    let device = dev.into_inner();

    // ── Scenario D2: same 1 MiB but in ONE write_file call ──────────────────
    // A single write_file of a contiguous multi-block run is one multi-block
    // device write (write-through `write_run`). Contrast with Scenario D where
    // the *caller* splits the same 1 MiB into 256 separate 4 KiB writes.
    ctr.reset();
    let (mut dev, mut fs) = cold_mount(device);
    mkfile(&mut dev, &mut fs, "/big2.bin", None, None).expect("mkfile big2");
    ctr.reset();
    let bigbuf = vec![0xCDu8; BIG_FILE_BYTES];
    write_file(&mut dev, &mut fs, "/big2.bin", 0, &bigbuf).expect("write_file 1MiB");
    let bw2_s = ctr.snapshot();
    eprintln!("[D2 write1call ] {}", bw2_s.fmt());
    eprintln!(
        "    1 MiB in a single write_file call → {} wOps (vs {} for 256×4 KiB)",
        bw2_s.write_ops, n_chunks
    );
    ctr.reset();
    umount(fs, &mut dev).expect("umount");
    let d2_commit = ctr.snapshot();
    eprintln!("[  umount commit] {}", d2_commit.fmt());
    let mut device = dev.into_inner();

    // ── Scenario G: 1 MiB append at varied chunk sizes ───────────────────────
    // Write-performance focus: same 1 MiB, different caller chunk sizes. The
    // write-back path defers single-block (4 KiB) writes so they coalesce at
    // flush; larger chunks (≥2 blocks) stay write-through (1 multi-block IOP
    // per call). Shows the breakpoint and that small-write throughput is no
    // longer 1 IOP/block.
    eprintln!();
    eprintln!("── write throughput vs chunk size (1 MiB append) ──");
    for &chunk_kib in &[4usize, 16, 64, 256, 1024] {
        let chunk_bytes = chunk_kib * 1024;
        let (mut dev, mut fs) = cold_mount(device);
        let path = format!("/w_{}.bin", chunk_kib);
        mkfile(&mut dev, &mut fs, &path, None, None).expect("mkfile");
        ctr.reset();
        let chunk = vec![0xCDu8; chunk_bytes];
        let mut off = 0u64;
        while off < BIG_FILE_BYTES as u64 {
            write_file(&mut dev, &mut fs, &path, off, &chunk).expect("write_file");
            off += chunk_bytes as u64;
        }
        let w = ctr.snapshot();
        ctr.reset();
        umount(fs, &mut dev).expect("umount");
        let c = ctr.snapshot();
        device = dev.into_inner();
        let total_us = w.modeled_us + c.modeled_us;
        eprintln!(
            "  chunk {:>4} KiB: write {:>7} | commit {:>7} | total {:>6.3} s ({:.2} MiB/s effective)",
            chunk_kib,
            w.fmt(),
            c.fmt(),
            total_us as f64 / 1_000_000.0,
            (BIG_FILE_BYTES as f64 / (1024.0 * 1024.0)) / (total_us as f64 / 1_000_000.0),
        );
    }

    // ── Scenario H: 4 MiB in 4 KiB chunks (exceeds the 1 MiB data cache) ──────
    // Tests the eviction fallback: once the dirty cache fills, victims are
    // written back. With the no-read eviction fix this is 1 IOP/block (not 2),
    // and the tail still coalesces at umount.
    {
        let big4 = 4 * BIG_FILE_BYTES;
        let (mut dev, mut fs) = cold_mount(device);
        mkfile(&mut dev, &mut fs, "/h.bin", None, None).expect("mkfile");
        ctr.reset();
        let chunk = vec![0xCDu8; BLOCK_SIZE];
        let mut off = 0u64;
        while off < big4 as u64 {
            write_file(&mut dev, &mut fs, "/h.bin", off, &chunk).expect("write_file");
            off += BLOCK_SIZE as u64;
        }
        let w = ctr.snapshot();
        ctr.reset();
        umount(fs, &mut dev).expect("umount");
        let c = ctr.snapshot();
        device = dev.into_inner();
        let total_us = w.modeled_us + c.modeled_us;
        eprintln!(
            "[H 4MiB 4K-chunk] write {:>7} | commit {:>7} | total {:>6.3} s ({:.2} MiB/s effective)",
            w.fmt(),
            c.fmt(),
            total_us as f64 / 1_000_000.0,
            (big4 as f64 / (1024.0 * 1024.0)) / (total_us as f64 / 1_000_000.0),
        );
    }

    // ── Scenario E: cold mount + create 50 dirs + commit (metadata alloc) ───
    ctr.reset();
    let (mut dev, mut fs) = cold_mount(device);
    ctr.reset();
    for i in 0..N_DIRS {
        mkdir(&mut dev, &mut fs, &format!("/d_{:03}", i)).expect("mkdir");
    }
    let mkdir_s = ctr.snapshot();
    eprintln!("[E mkdir {:>3} ] {}", N_DIRS, mkdir_s.fmt());
    eprintln!(
        "    per-dir: reads={:.2} writes={:.2} modeled={:.2} ms",
        mkdir_s.read_ops as f64 / N_DIRS as f64,
        mkdir_s.write_ops as f64 / N_DIRS as f64,
        mkdir_s.modeled_us as f64 / N_DIRS as f64 / 1000.0,
    );
    ctr.reset();
    umount(fs, &mut dev).expect("umount");
    let dircommit_s = ctr.snapshot();
    eprintln!("[  umount commit] {}", dircommit_s.fmt());
    let device = dev.into_inner();

    // ── Scenario F: cold mount + delete 100 files + commit ──────────────────
    ctr.reset();
    let (mut dev, mut fs) = cold_mount(device);
    ctr.reset();
    for i in 0..N_FILES {
        delete_file(&mut fs, &mut dev, &file_path(i)).expect("delete_file");
    }
    let del_s = ctr.snapshot();
    eprintln!("[F delete {:>3} ] {}", N_FILES, del_s.fmt());
    eprintln!(
        "    per-file: reads={:.2} writes={:.2} modeled={:.2} ms",
        del_s.read_ops as f64 / N_FILES as f64,
        del_s.write_ops as f64 / N_FILES as f64,
        del_s.modeled_us as f64 / N_FILES as f64 / 1000.0,
    );
    ctr.reset();
    umount(fs, &mut dev).expect("umount");
    let delcommit_s = ctr.snapshot();
    eprintln!("[  umount commit] {}", delcommit_s.fmt());

    eprintln!();
    eprintln!("Hot blocks during 1 MiB write (Scenario D), top-12 by writes:");
    // Re-run D briefly is expensive; instead report hot blocks from last reset (delete commit).
    eprintln!("(reported for delete-commit; top-12 by writes)");
    for (blk, n) in ctr.top(12, true) {
        eprintln!("   block {:>6} : {} writes", blk, n);
    }
    eprintln!("############################################################################");
}

fn fmt_us(us: u64) -> String {
    format!("{:.3} s", us as f64 / 1_000_000.0)
}

/// File-creation profiling: create many files and report per-file I/O plus the
/// hot read blocks (to see exactly what is re-read per file).
#[test]
#[ignore = "profiling harness, run explicitly"]
fn profile_file_creation() {
    eprintln!();
    eprintln!("############################################################################");
    eprintln!("# rsext4 file-creation profiling  (read={}us write={}us; bw r={}B/s w={}B/s)",
        READ_LATENCY_US, WRITE_LATENCY_US, READ_BW_BYTES_PER_S, WRITE_BW_BYTES_PER_S);
    eprintln!("############################################################################");
    let ctr = Arc::new(Counters::new());

    for &with_data in &[false, true] {
        for &n in &[200usize] {
            let device = ProfilingBlockDevice::new(100 * 1024 * 1024, ctr.clone());
            let mut dev = Jbd2Dev::initial_jbd2dev(0, device, true);
            mkfs(&mut dev).expect("mkfs");
            let mut fs = mount(&mut dev).expect("mount");
            mkdir(&mut dev, &mut fs, "/files").expect("mkdir");
            ctr.reset(); // count only the creates

            let payload = vec![0xABu8; 512];
            for i in 0..n {
                let path = format!("/files/f_{:04}", i);
                let data = if with_data { Some(payload.as_slice()) } else { None };
                mkfile(&mut dev, &mut fs, &path, data, None).expect("mkfile");
            }
            let create_s = ctr.snapshot();
            eprintln!(
                "[create {:>3} {}] {}  per-file: reads={:.2} writes={:.2} modeled={:.3} ms",
                n,
                if with_data { "512B " } else { "empty" },
                create_s.fmt(),
                create_s.read_ops as f64 / n as f64,
                create_s.write_ops as f64 / n as f64,
                create_s.modeled_us as f64 / n as f64 / 1000.0,
            );
            eprintln!("    top-12 most-read blocks during create:");
            for (blk, c) in ctr.top(12, false) {
                eprintln!("       block {:>6} : {} reads", blk, c);
            }
            ctr.reset();
            umount(fs, &mut dev).expect("umount");
            let commit_s = ctr.snapshot();
            eprintln!("[  commit     ] {}  per-file: modeled={:.3} ms",
                commit_s.fmt(),
                commit_s.modeled_us as f64 / n as f64 / 1000.0,
            );
            eprintln!();
        }
    }
    eprintln!("############################################################################");
}

