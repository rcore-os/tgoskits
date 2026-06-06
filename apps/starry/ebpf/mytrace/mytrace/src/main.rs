use std::{ffi::CString, time::Duration};

use aya::{maps::HashMap, programs::TracePoint};
#[rustfmt::skip]
use log::{debug, warn};

fn main() -> anyhow::Result<()> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Warn)
        .format_timestamp(None)
        .init();

    // Bump the memlock rlimit, see https://lwn.net/Articles/837122/
    let rlim = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    let ret = unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &rlim) };
    if ret != 0 {
        debug!("remove limit on locked memory failed, ret is: {ret}");
    }

    let mut ebpf = aya::Ebpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/mytrace"
    )))?;

    // aya resolves the tracepoint id through
    // /sys/kernel/debug/tracing/events/syscalls/sys_enter_openat/id, which
    // StarryOS exposes in debugfs.
    let program: &mut TracePoint = ebpf.program_mut("mytrace").unwrap().try_into()?;
    program.load()?;
    program.attach("syscalls", "sys_enter_openat")?;
    println!("MYTRACE: attached tracepoint syscalls:sys_enter_openat");

    // Deterministic workload: a fixed number of openat(2) calls. Each one fires
    // the sys_enter_openat tracepoint, bumping OPENAT_HITS[0].
    const N: u64 = 200;
    let path = CString::new("/etc/hostname").unwrap();
    println!("MYTRACE: issuing {N} openat() calls");
    for _ in 0..N {
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY) };
        if fd >= 0 {
            unsafe { libc::close(fd) };
        }
    }
    std::thread::sleep(Duration::from_millis(500));

    let map: HashMap<_, u32, u64> =
        HashMap::try_from(ebpf.map("OPENAT_HITS").expect("OPENAT_HITS missing"))?;
    let hits = map.get(&0u32, 0).unwrap_or(0);
    println!("MYTRACE: tracepoint fired {hits} times (drove {N} openat calls)");

    // Anti-fallback: the tracepoint must fire for our workload (generous slack;
    // other processes' opens only add to the count). Not merely non-zero.
    let threshold = N / 2;
    if hits >= threshold {
        println!("MYTRACE_PASS: tracepoint fired {hits} times (>= {threshold})");
        Ok(())
    } else {
        warn!("tracepoint fired too few times: {hits} < {threshold}");
        println!("MYTRACE_FAIL: tracepoint fired {hits} times (< {threshold})");
        std::process::exit(1);
    }
}
