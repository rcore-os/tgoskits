//! M1 round-trip coverage for all five controllers.
//!
//! These tests drive each controller through its *public* trait surface
//! (`CgroupControllerFactory::new_instance` → `write_attr` / `read_attr`),
//! exactly as the VFS layer does. Parser/formatter internals stay private;
//! we assert on observable read-back text and on the error codes Linux
//! userspace would see.

use alloc::string::String;

use axfs_ng_vfs::VfsError;

use crate::{
    controller::{CgroupController, CgroupControllerFactory},
    cpu::CpuControllerFactory,
    cpuset::CpusetControllerFactory,
    io::IoControllerFactory,
    memory::MemoryControllerFactory,
    pids::PidsControllerFactory,
};

/// Read an attribute fully into an owned `String`.
fn read_str(ctrl: &dyn CgroupController, name: &str) -> String {
    let mut buf = [0u8; 256];
    let n = ctrl
        .read_attr(name, 0, &mut buf)
        .expect("read_attr should succeed");
    String::from_utf8(buf[..n].to_vec()).expect("attr output is utf-8")
}

/// Write a string value to an attribute.
fn write(ctrl: &dyn CgroupController, name: &str, val: &str) -> Result<usize, VfsError> {
    ctrl.write_attr(name, val.as_bytes())
}

// ── pids ─────────────────────────────────────────────────────────────

#[test]
fn pids_max_round_trip() {
    let ctrl = PidsControllerFactory.new_instance();

    // Default is unlimited.
    assert_eq!(read_str(&*ctrl, "max"), "max\n");

    // A concrete limit reads back verbatim.
    write(&*ctrl, "max", "10").unwrap();
    assert_eq!(read_str(&*ctrl, "max"), "10\n");

    // "max" restores unlimited.
    write(&*ctrl, "max", "max").unwrap();
    assert_eq!(read_str(&*ctrl, "max"), "max\n");
}

#[test]
fn pids_max_rejects_negative() {
    let ctrl = PidsControllerFactory.new_instance();
    assert_eq!(write(&*ctrl, "max", "-5"), Err(VfsError::InvalidInput));
    assert_eq!(write(&*ctrl, "max", "abc"), Err(VfsError::InvalidInput));
}

#[test]
fn pids_current_is_read_only() {
    let ctrl = PidsControllerFactory.new_instance();
    assert_eq!(
        write(&*ctrl, "current", "1"),
        Err(VfsError::OperationNotPermitted)
    );
}

// ── memory ───────────────────────────────────────────────────────────

#[test]
fn memory_max_round_trip_units() {
    let ctrl = MemoryControllerFactory.new_instance();

    // Default unlimited prints "max".
    assert_eq!(read_str(&*ctrl, "max"), "max\n");

    // Suffixes are normalised to bytes on read-back.
    write(&*ctrl, "max", "1K").unwrap();
    assert_eq!(read_str(&*ctrl, "max"), "1024\n");
    write(&*ctrl, "max", "512M").unwrap();
    assert_eq!(read_str(&*ctrl, "max"), "536870912\n");
    write(&*ctrl, "max", "1G").unwrap();
    assert_eq!(read_str(&*ctrl, "max"), "1073741824\n");
    write(&*ctrl, "max", "1048576").unwrap();
    assert_eq!(read_str(&*ctrl, "max"), "1048576\n");

    // "max" restores unlimited.
    write(&*ctrl, "max", "max").unwrap();
    assert_eq!(read_str(&*ctrl, "max"), "max\n");
}

#[test]
fn memory_max_rejects_negative() {
    let ctrl = MemoryControllerFactory.new_instance();
    assert_eq!(write(&*ctrl, "max", "-1"), Err(VfsError::InvalidInput));
    assert_eq!(write(&*ctrl, "max", "nope"), Err(VfsError::InvalidInput));
}

#[test]
fn memory_events_format() {
    let ctrl = MemoryControllerFactory.new_instance();
    // Fresh controller: all counters zero, three labelled lines.
    assert_eq!(read_str(&*ctrl, "events"), "max 0\nhigh 0\noom 0\n");
}

#[test]
fn memory_current_and_events_read_only() {
    let ctrl = MemoryControllerFactory.new_instance();
    assert_eq!(
        write(&*ctrl, "current", "1"),
        Err(VfsError::OperationNotPermitted)
    );
    assert_eq!(
        write(&*ctrl, "events", "x"),
        Err(VfsError::OperationNotPermitted)
    );
}

// ── cpu ──────────────────────────────────────────────────────────────

#[test]
fn cpu_weight_bounds() {
    let ctrl = CpuControllerFactory.new_instance();
    assert_eq!(read_str(&*ctrl, "weight"), "100\n");

    write(&*ctrl, "weight", "1").unwrap();
    assert_eq!(read_str(&*ctrl, "weight"), "1\n");
    write(&*ctrl, "weight", "10000").unwrap();
    assert_eq!(read_str(&*ctrl, "weight"), "10000\n");

    // Out of [1, 10000] is rejected.
    assert_eq!(write(&*ctrl, "weight", "0"), Err(VfsError::InvalidInput));
    assert_eq!(
        write(&*ctrl, "weight", "10001"),
        Err(VfsError::InvalidInput)
    );
}

#[test]
fn cpu_max_round_trip() {
    let ctrl = CpuControllerFactory.new_instance();
    // Default: no quota, default period.
    assert_eq!(read_str(&*ctrl, "max"), "max 100000\n");

    // Quota + explicit period.
    write(&*ctrl, "max", "50000 100000").unwrap();
    assert_eq!(read_str(&*ctrl, "max"), "50000 100000\n");

    // Single value keeps the previous period.
    write(&*ctrl, "max", "max").unwrap();
    assert_eq!(read_str(&*ctrl, "max"), "max 100000\n");
}

#[test]
fn cpu_max_rejects_bad_values() {
    let ctrl = CpuControllerFactory.new_instance();
    // quota <= 0
    assert_eq!(
        write(&*ctrl, "max", "0 100000"),
        Err(VfsError::InvalidInput)
    );
    // period out of [1000, 1_000_000]
    assert_eq!(
        write(&*ctrl, "max", "1000 100"),
        Err(VfsError::InvalidInput)
    );
    assert_eq!(
        write(&*ctrl, "max", "1000 2000000"),
        Err(VfsError::InvalidInput)
    );
    // too many fields
    assert_eq!(write(&*ctrl, "max", "1 2 3"), Err(VfsError::InvalidInput));
}

#[test]
fn cpu_stat_format() {
    let ctrl = CpuControllerFactory.new_instance();
    assert_eq!(
        read_str(&*ctrl, "stat"),
        "nr_periods 0\nnr_throttled 0\nthrottled_usec 0\n"
    );
}

// ── cpuset ───────────────────────────────────────────────────────────

#[test]
fn cpuset_cpus_round_trip() {
    let ctrl = CpusetControllerFactory.new_instance();

    write(&*ctrl, "cpus", "0-3,5,7").unwrap();
    assert_eq!(read_str(&*ctrl, "cpus"), "0-3,5,7\n");

    // Single cpu.
    write(&*ctrl, "cpus", "2").unwrap();
    assert_eq!(read_str(&*ctrl, "cpus"), "2\n");

    // Adjacent values collapse into a range on read-back.
    write(&*ctrl, "cpus", "0,1,2,3").unwrap();
    assert_eq!(read_str(&*ctrl, "cpus"), "0-3\n");
}

#[test]
fn cpuset_cpus_rejects_bad_lists() {
    let ctrl = CpusetControllerFactory.new_instance();
    // out of range (>= 64)
    assert_eq!(write(&*ctrl, "cpus", "64"), Err(VfsError::InvalidInput));
    // start > end
    assert_eq!(write(&*ctrl, "cpus", "5-2"), Err(VfsError::InvalidInput));
    // non-numeric
    assert_eq!(write(&*ctrl, "cpus", "a-b"), Err(VfsError::InvalidInput));
}

#[test]
fn cpuset_effective_is_read_only() {
    let ctrl = CpusetControllerFactory.new_instance();
    assert_eq!(
        write(&*ctrl, "cpus.effective", "0-1"),
        Err(VfsError::OperationNotPermitted)
    );
}

// ── io ───────────────────────────────────────────────────────────────

#[test]
fn io_weight_bounds() {
    let ctrl = IoControllerFactory.new_instance();
    assert_eq!(read_str(&*ctrl, "weight"), "100\n");

    write(&*ctrl, "weight", "500").unwrap();
    assert_eq!(read_str(&*ctrl, "weight"), "500\n");

    assert_eq!(write(&*ctrl, "weight", "0"), Err(VfsError::InvalidInput));
    assert_eq!(
        write(&*ctrl, "weight", "10001"),
        Err(VfsError::InvalidInput)
    );
}

#[test]
fn io_max_accepts_valid_lines() {
    let ctrl = IoControllerFactory.new_instance();
    // Well-formed device limit line parses.
    write(&*ctrl, "max", "8:0 rbps=1048576 wbps=max").unwrap();
    // "default" and blank lines are tolerated.
    write(&*ctrl, "max", "default").unwrap();
}

#[test]
fn io_max_rejects_bad_lines() {
    let ctrl = IoControllerFactory.new_instance();
    // Unknown key.
    assert_eq!(
        write(&*ctrl, "max", "8:0 foo=1"),
        Err(VfsError::InvalidInput)
    );
    // Missing device.
    assert_eq!(write(&*ctrl, "max", "rbps=1"), Err(VfsError::InvalidInput));
}
