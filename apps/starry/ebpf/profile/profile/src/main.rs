use std::{
    process,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use aya::{maps::HashMap, programs::KProbe};

/// Set from the SIGTERM/SIGINT handler so the main thread breaks out of its
/// poll loop, dumps the histogram, and exits cleanly (which drops the loaded
/// program and detaches the kprobe).
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: i32) {
    STOP.store(true, Ordering::SeqCst);
}

fn main() -> anyhow::Result<()> {
    // Usage: profile <mangled_handle_syscall_symbol>
    //
    // The symbol is resolved by the kernel through /proc/kallsyms, so it must
    // be the *mangled* name. Get it with:
    //   SYM=$(grep -m1 handle_syscall /proc/kallsyms | awk '{print $3}')
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: profile <mangled_handle_syscall_symbol>");
        eprintln!(
            "  e.g. SYM=$(grep -m1 handle_syscall /proc/kallsyms | awk '{{print $3}}'); profile \
             \"$SYM\""
        );
        process::exit(2);
    }
    let symbol = &args[1];

    // Bump RLIMIT_MEMLOCK so map allocation is not capped on kernels that
    // still use rlimit-based BPF memory accounting.
    let rlim = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    // SAFETY: a valid `rlimit` for a known resource id.
    unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &rlim) };

    // Dump-and-exit on SIGTERM (test harness) / SIGINT (Ctrl-C).
    // SAFETY: `on_signal` is async-signal-safe (a single atomic store).
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as usize);
        libc::signal(libc::SIGINT, on_signal as *const () as usize);
    }

    let mut ebpf = aya::Ebpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/profile"
    )))?;

    let program: &mut KProbe = ebpf
        .program_mut("profile")
        .expect("profile program missing")
        .try_into()?;
    program.load()?;
    program.attach(symbol, 0)?;
    // The test harness waits for this line before driving the workload.
    println!("profile: attached kprobe to {symbol}");

    // Profile until signalled. No fixed deadline: the harness controls the
    // window by sending SIGTERM after its workload finishes.
    while !STOP.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(100));
    }

    dump_histogram(&ebpf)?;
    Ok(())
}

/// Iterate the syscall histogram, rank it by hit count, and print a parseable
/// report. The trailing `PROFILE_END` summary line carries the aggregates the
/// verification script asserts on.
fn dump_histogram(ebpf: &aya::Ebpf) -> anyhow::Result<()> {
    let hist: HashMap<_, u32, u64> =
        HashMap::try_from(ebpf.map("SYSCALL_HIST").expect("SYSCALL_HIST map missing"))?;

    let mut rows: Vec<(u32, u64)> = Vec::new();
    let mut total: u64 = 0;
    for item in hist.iter() {
        if let Ok((sysno, count)) = item {
            total += count;
            rows.push((sysno, count));
        }
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
    Ok(())
}

fn pct_of(count: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (count as f64) * 100.0 / (total as f64)
    }
}
