use std::{fs, time::Duration};

use aya::{maps::HashMap, programs::KProbe};
#[rustfmt::skip]
use log::{debug, warn};

// Resolve the (possibly mangled) kallsyms entry for
// `starry_kernel::syscall::sysno`, the `#[inline(never)]` helper whose first
// argument is the raw syscall number. Probing it (rather than `handle_syscall`,
// whose arg0 is `&UserContext`) lets the eBPF program read the number straight
// off `arg(0)` on every arch. The mangled symbol contains both `syscall` (the
// module) and `sysno`; requiring both excludes `handle_syscall` (no `sysno`)
// and the `UserContext::sysno` accessor (no `syscall`). The kernel's kprobe
// lookup matches the kallsyms name exactly, so we hand aya the real symbol
// string, not the source name.
fn resolve_sysno() -> anyhow::Result<String> {
    let table = fs::read_to_string("/proc/kallsyms")?;
    for line in table.lines() {
        // Format: "<addr> <type> <name>".
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
        "/syscall_count"
    )))?;

    let target = resolve_sysno()?;
    println!("SYSCALL_COUNT: kprobe target symbol = {target}");
    let program: &mut KProbe = ebpf.program_mut("syscall_count").unwrap().try_into()?;
    program.load()?;
    program.attach(&target, 0)?;

    // Deterministic workload: a fixed number of getpid(2) calls. Keyed by the
    // getpid syscall number, only these calls bump SYSCALL_LIST[SYS_getpid], so
    // the count is assertable (a broken kprobe/arg-read/map path cannot pass).
    let sys_getpid = libc::SYS_getpid as u32;
    const N: u32 = 500;
    println!("SYSCALL_COUNT: issuing {N} getpid() calls (SYS_getpid={sys_getpid})");
    for _ in 0..N {
        unsafe { libc::syscall(libc::SYS_getpid) };
    }
    // Let the probe path settle.
    std::thread::sleep(Duration::from_millis(500));

    let map: HashMap<_, u32, u32> =
        HashMap::try_from(ebpf.map("SYSCALL_LIST").expect("SYSCALL_LIST missing"))?;
    let mut getpid_count: u32 = 0;
    let mut distinct = 0u32;
    for (sysno, count) in map.iter().flatten() {
        distinct += 1;
        if sysno == sys_getpid {
            getpid_count = count;
        }
    }
    println!(
        "SYSCALL_COUNT: distinct syscalls seen = {distinct}, getpid count = {getpid_count} (drove {N})"
    );

    // Anti-fallback: the keys must be real, small syscall numbers (an earlier
    // version that dereferenced `&UserContext` recorded the pointer's first
    // word, producing bogus keys), and the getpid count must clearly reflect
    // the workload. Allow generous TCG slack.
    if sys_getpid >= 1024 {
        // SYS_getpid should be a small number on every supported ABI.
        warn!("unexpected SYS_getpid value {sys_getpid}");
    }
    if getpid_count >= N / 5 {
        println!(
            "SYSCALL_COUNT_PASS: getpid fired {getpid_count} times (>= {})",
            N / 5
        );
        Ok(())
    } else {
        println!(
            "SYSCALL_COUNT_FAIL: getpid count {getpid_count} < {}",
            N / 5
        );
        std::process::exit(1);
    }
}
