use std::{fs, thread, time::Duration};

use aya::{maps::HashMap, programs::KProbe};

// Resolve the (possibly mangled) kallsyms entry for
// `starry_kernel::syscall::sysno`, the `#[inline(never)]` helper whose first
// argument is the raw syscall number. Probing it (rather than `handle_syscall`,
// whose arg0 is `&UserContext`) lets the eBPF program read the number straight
// off `arg(0)` on every arch. The mangled symbol contains both `syscall` (the
// module) and `sysno`; requiring both excludes `handle_syscall` (no `sysno`)
// and the `UserContext::sysno` accessor (no `syscall`). The kernel's kprobe
// lookup matches the kallsyms name exactly, so hand aya the real symbol string.
fn resolve_sysno() -> anyhow::Result<String> {
    let table = fs::read_to_string("/proc/kallsyms")?;
    for line in table.lines() {
        if let Some(name) = line.split_whitespace().nth(2)
            && name.contains("syscall")
            && name.contains("sysno")
        {
            return Ok(name.to_string());
        }
    }
    anyhow::bail!("syscall::sysno not found in /proc/kallsyms")
}

fn main() -> anyhow::Result<()> {
    // Bump RLIMIT_MEMLOCK so map allocation is not capped on kernels that
    // still use rlimit-based BPF memory accounting.
    let rlim = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    // SAFETY: a valid `rlimit` for a known resource id.
    unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &rlim) };

    let mut ebpf = aya::Ebpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/profile"
    )))?;

    let symbol = resolve_sysno()?;
    let program: &mut KProbe = ebpf
        .program_mut("profile")
        .expect("profile program missing")
        .try_into()?;
    program.load()?;
    program.attach(&symbol, 0)?;
    println!("PROFILE: attached kprobe to {symbol}");

    // Drive a varied syscall workload so the histogram spans many distinct
    // syscall numbers (not just one hot key).
    let workload = thread::spawn(|| drive_workload(Duration::from_secs(6)));
    workload.join().ok();
    // Let the last probe hits settle into the map.
    thread::sleep(Duration::from_millis(300));

    dump_histogram(&ebpf)
}

/// Issue a mix of syscalls repeatedly so the profile is non-trivial.
fn drive_workload(dur: Duration) {
    let deadline = std::time::Instant::now() + dur;
    while std::time::Instant::now() < deadline {
        unsafe {
            libc::syscall(libc::SYS_getpid);
            libc::syscall(libc::SYS_getuid);
            libc::syscall(libc::SYS_getgid);
            libc::syscall(libc::SYS_gettid);
            // sched_yield + nanosleep add scheduling-related syscalls.
            libc::sched_yield();
        }
        let ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 1_000_000,
        };
        unsafe { libc::nanosleep(&ts, std::ptr::null_mut()) };
    }
}

/// Iterate the syscall histogram, rank it by hit count, and print a parseable
/// report. The trailing `PROFILE_END` summary line carries the aggregates the
/// assertion checks.
fn dump_histogram(ebpf: &aya::Ebpf) -> anyhow::Result<()> {
    let hist: HashMap<_, u32, u64> =
        HashMap::try_from(ebpf.map("SYSCALL_HIST").expect("SYSCALL_HIST map missing"))?;

    let mut rows: Vec<(u32, u64)> = Vec::new();
    let mut total: u64 = 0;
    for (sysno, count) in hist.iter().flatten() {
        total += count;
        rows.push((sysno, count));
    }
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    println!("PROFILE_BEGIN");
    for (sysno, count) in &rows {
        let pct = pct_of(*count, total);
        println!("syscall {sysno} count {count} pct {pct:.1}");
    }
    let distinct = rows.len();
    let (top1_sysno, top1_count) = rows.first().copied().unwrap_or((0, 0));
    let top1_pct = pct_of(top1_count, total);
    println!(
        "PROFILE_END total={total} distinct={distinct} top1_sysno={top1_sysno} \
         top1_count={top1_count} top1_pct={top1_pct:.1}"
    );

    // Anti-fallback: a working `sysno` kprobe must produce a histogram with
    // many samples spanning several small, real syscall numbers (an earlier
    // version that dereferenced `&UserContext` recorded huge pointer-word keys).
    let all_small = rows.iter().all(|(sysno, _)| *sysno < 1024);
    if total >= 50 && distinct >= 3 && all_small {
        println!("PROFILE_PASS: total={total} distinct={distinct}");
        Ok(())
    } else {
        println!("PROFILE_FAIL: total={total} distinct={distinct} all_small={all_small}");
        std::process::exit(1);
    }
}

fn pct_of(count: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (count as f64) * 100.0 / (total as f64)
    }
}
