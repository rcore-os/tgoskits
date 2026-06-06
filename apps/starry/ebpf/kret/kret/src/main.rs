use std::{fs, time::Duration};

use aya::{maps::HashMap, programs::KProbe};
#[rustfmt::skip]
use log::{debug, warn};

// Resolve the (possibly mangled) kallsyms entry for `sys_getpid`. The kernel's
// kprobe lookup matches the kallsyms name exactly, so hand aya the real symbol.
fn resolve_sym(needle: &str) -> anyhow::Result<String> {
    let table = fs::read_to_string("/proc/kallsyms")?;
    for line in table.lines() {
        if let Some(name) = line.split_whitespace().nth(2)
            && name.contains(needle)
        {
            return Ok(name.to_string());
        }
    }
    anyhow::bail!("{needle} not found in /proc/kallsyms")
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
        "/kret"
    )))?;

    let target = resolve_sym("sys_getpid")?;
    println!("KRET: kretprobe target symbol = {target}");
    let program: &mut KProbe = ebpf.program_mut("kret").unwrap().try_into()?;
    program.load()?;
    // A kretprobe-typed program attaches as a return probe.
    program.attach(&target, 0)?;

    // Deterministic workload: a fixed number of getpid(2) calls, each of which
    // returns through sys_getpid and must trip the kretprobe.
    const N: u64 = 500;
    println!("KRET: issuing {N} getpid() calls");
    for _ in 0..N {
        unsafe { libc::syscall(libc::SYS_getpid) };
    }
    std::thread::sleep(Duration::from_millis(500));

    let map: HashMap<_, u32, u64> =
        HashMap::try_from(ebpf.map("KRET_HITS").expect("KRET_HITS missing"))?;
    let hits = map.get(&0u32, 0).unwrap_or(0);
    println!("KRET: kretprobe fired {hits} times (drove {N} getpid calls)");

    // Anti-fallback: require the return-probe count to clearly reflect the
    // workload (generous TCG slack), not merely be non-zero.
    let threshold = N / 5;
    if hits >= threshold {
        println!("KRET_PASS: kretprobe fired {hits} times (>= {threshold})");
        Ok(())
    } else {
        warn!("kretprobe fired too few times: {hits} < {threshold}");
        println!("KRET_FAIL: kretprobe fired {hits} times (< {threshold})");
        std::process::exit(1);
    }
}
