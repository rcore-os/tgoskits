use std::time::Duration;

use aya::{
    maps::HashMap,
    programs::{UProbe, uprobe::UProbeScope},
};
use clap::Parser;
#[rustfmt::skip]
use log::{debug, warn};

#[derive(Debug, Parser)]
struct Opt {
    /// Path to the ELF that hosts `uprobe_test`. Defaults to this executable.
    #[clap(long)]
    target: Option<String>,

    /// How long to drive the workload before reading the map back, in seconds.
    #[clap(long, default_value_t = 8)]
    secs: u64,
}

// The function the uprobe is attached to. `#[inline(never)]` + `#[no_mangle]`
// keep it as a real, named symbol so aya can resolve its file offset.
#[inline(never)]
#[unsafe(no_mangle)]
fn uprobe_test(a: u32, b: Option<u32>) -> u32 {
    std::hint::black_box(a).wrapping_add(b.unwrap_or(0))
}

// The argument the workload always passes; the loader asserts the HashMap key.
const PROBE_ARG: u32 = 42;

fn main() -> anyhow::Result<()> {
    let opt = Opt::parse();

    env_logger::builder()
        .filter_level(log::LevelFilter::Warn)
        .format_timestamp(None)
        .init();

    // Bump the memlock rlimit. This is needed for older kernels that don't use the
    // new memcg based accounting, see https://lwn.net/Articles/837122/
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
        "/upb"
    )))?;

    let program: &mut UProbe = ebpf.program_mut("upb").unwrap().try_into()?;
    program.load()?;

    // The kernel resolves a uprobe by matching the perf-event's ELF path against
    // a file-backed VMA in the target process, so the target must be the exact
    // path this process was exec'd from. Default to the current executable.
    let target = match opt.target {
        Some(t) => t,
        None => std::env::current_exe()?.to_string_lossy().into_owned(),
    };
    let pid = std::process::id() as i32;
    println!(
        "UPROBE: attaching to '{}' uprobe_test in pid {}",
        target, pid
    );
    println!(
        "UPROBE: uprobe_test symbol addr = {:#x}",
        uprobe_test as *const () as usize
    );
    // Warm up: execute the probed function once so its text page is faulted in
    // before arming. The kernel reads the original instruction bytes and plants
    // the breakpoint through that page's physical frame, so it must be resident.
    let _ = uprobe_test(PROBE_ARG, Some(58));
    program.attach("uprobe_test", &target, UProbeScope::CallingProcess)?;

    // Drive the workload: call uprobe_test in a tight-ish loop so the uprobe
    // fires a deterministic number of times.
    let deadline = std::time::Instant::now() + Duration::from_secs(opt.secs);
    let mut calls: u64 = 0;
    while std::time::Instant::now() < deadline {
        uprobe_test(PROBE_ARG, Some(58));
        calls += 1;
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("UPROBE: issued {} uprobe_test({}) calls", calls, PROBE_ARG);

    // Read the HashMap back: the uprobe bumps UPROBE_HITS[arg] on every hit.
    let hits: HashMap<_, u32, u64> =
        HashMap::try_from(ebpf.map("UPROBE_HITS").expect("UPROBE_HITS map missing"))?;
    let mut max_count: u64 = 0;
    for (key, value) in hits.iter().flatten() {
        println!("UPROBE: hits for arg={} -> {}", key, value);
        if key == PROBE_ARG {
            max_count = value;
        }
    }

    // Anti-fallback assertion: require the recorded count to clearly reflect the
    // workload (generous TCG slack), not merely be non-zero.
    let threshold = (calls / 2).max(10);
    if max_count >= threshold {
        println!(
            "UPROBE_PASS: arg={} fired {} times (>= {})",
            PROBE_ARG, max_count, threshold
        );
        Ok(())
    } else {
        warn!("uprobe did not fire enough: {max_count} < {threshold}");
        println!(
            "UPROBE_FAIL: arg={} fired {} times (< {})",
            PROBE_ARG, max_count, threshold
        );
        std::process::exit(1);
    }
}
