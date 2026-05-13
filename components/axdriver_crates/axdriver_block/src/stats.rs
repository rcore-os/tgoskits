//! Lightweight block I/O accounting for procfs-style disk statistics.

use alloc::{string::String, vec::Vec};

use spin::{Lazy, Mutex};

const LINUX_SECTOR_SIZE: u64 = 512;

static DISKS: Lazy<Mutex<Vec<BlockStats>>> = Lazy::new(|| Mutex::new(Vec::new()));

/// A snapshot of one block device's I/O counters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockStatsSnapshot {
    /// Internal block-device major number.
    pub major: u32,
    /// Internal block-device minor number.
    pub minor: u32,
    /// Device name reported by the backing driver.
    pub name: String,
    /// Completed read requests.
    pub reads_completed: u64,
    /// Merged read requests. ArceOS does not merge block requests yet.
    pub reads_merged: u64,
    /// 512-byte sectors read.
    pub sectors_read: u64,
    /// Milliseconds spent reading. Not tracked by the current synchronous driver API.
    pub read_time_ms: u64,
    /// Completed write requests.
    pub writes_completed: u64,
    /// Merged write requests. ArceOS does not merge block requests yet.
    pub writes_merged: u64,
    /// 512-byte sectors written.
    pub sectors_written: u64,
    /// Milliseconds spent writing. Not tracked by the current synchronous driver API.
    pub write_time_ms: u64,
    /// I/O requests currently in progress.
    pub io_in_progress: u64,
    /// Milliseconds spent doing I/O. Not tracked by the current synchronous driver API.
    pub io_time_ms: u64,
    /// Weighted milliseconds spent doing I/O. Not tracked by the current synchronous driver API.
    pub weighted_io_time_ms: u64,
}

#[derive(Clone, Debug)]
struct BlockStats {
    major: u32,
    minor: u32,
    name: String,
    reads_completed: u64,
    sectors_read: u64,
    writes_completed: u64,
    sectors_written: u64,
}

impl BlockStats {
    fn snapshot(&self) -> BlockStatsSnapshot {
        BlockStatsSnapshot {
            major: self.major,
            minor: self.minor,
            name: self.name.clone(),
            reads_completed: self.reads_completed,
            reads_merged: 0,
            sectors_read: self.sectors_read,
            read_time_ms: 0,
            writes_completed: self.writes_completed,
            writes_merged: 0,
            sectors_written: self.sectors_written,
            write_time_ms: 0,
            io_in_progress: 0,
            io_time_ms: 0,
            weighted_io_time_ms: 0,
        }
    }
}

fn sectors_for_bytes(bytes: usize) -> u64 {
    (bytes as u64).div_ceil(LINUX_SECTOR_SIZE)
}

fn stats_index(disks: &mut Vec<BlockStats>, name: &str) -> usize {
    if let Some(index) = disks.iter().position(|stats| stats.name == name) {
        return index;
    }

    let minor = disks.len() as u32;
    disks.push(BlockStats {
        // StarryOS does not yet allocate Linux dev_t values for block devices.
        // Use an internal major and stable insertion-order minor while keeping
        // the driver-provided name and real I/O counters.
        major: 0,
        minor,
        name: name.into(),
        reads_completed: 0,
        sectors_read: 0,
        writes_completed: 0,
        sectors_written: 0,
    });
    disks.len() - 1
}

/// Registers a block device so it appears in disk statistics even before I/O.
pub fn register_device(name: &str) {
    let mut disks = DISKS.lock();
    let _ = stats_index(&mut disks, name);
}

/// Records one successfully completed block read request.
pub fn record_read(name: &str, bytes: usize) {
    if bytes == 0 {
        return;
    }

    let mut disks = DISKS.lock();
    let index = stats_index(&mut disks, name);
    let stats = &mut disks[index];
    stats.reads_completed = stats.reads_completed.saturating_add(1);
    stats.sectors_read = stats.sectors_read.saturating_add(sectors_for_bytes(bytes));
}

/// Records one successfully completed block write request.
pub fn record_write(name: &str, bytes: usize) {
    if bytes == 0 {
        return;
    }

    let mut disks = DISKS.lock();
    let index = stats_index(&mut disks, name);
    let stats = &mut disks[index];
    stats.writes_completed = stats.writes_completed.saturating_add(1);
    stats.sectors_written = stats
        .sectors_written
        .saturating_add(sectors_for_bytes(bytes));
}

/// Returns a point-in-time snapshot of all registered block devices.
pub fn snapshots() -> Vec<BlockStatsSnapshot> {
    DISKS.lock().iter().map(BlockStats::snapshot).collect()
}
