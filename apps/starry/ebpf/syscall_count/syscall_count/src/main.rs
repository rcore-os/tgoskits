use aya::{maps::HashMap, programs::KProbe};
use log::debug;
use std::{
    fs,
    io::{BufRead, BufReader},
};

use tokio::time;

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
    let buf = BufReader::new(fs::File::open("/proc/kallsyms")?);
    for line in buf.lines() {
        // Format: "<addr> <type> <name>".
        if let Some(name) = line?.split_whitespace().nth(2)
            && name.contains("syscall")
            && name.contains("sysno")
        {
            return Ok(name.to_string());
        }
    }
    anyhow::bail!("syscall::sysno not found in /proc/kallsyms")
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Warn)
        .format_timestamp(None)
        .init();

    let target_syscall_entry = resolve_sysno()?;

    println!("syscall_count: kprobe target symbol = {target_syscall_entry}");

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

    // This will include your eBPF object file as raw bytes at compile-time and load it at
    // runtime. This approach is recommended for most real-world use cases. If you would
    // like to specify the eBPF program at runtime rather than at compile-time, you can
    // reach for `Bpf::load_file` instead.
    let mut ebpf = aya::Ebpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/syscall_count"
    )))?;

    let program: &mut KProbe = ebpf.program_mut("syscall_ebpf").unwrap().try_into()?;
    program.load()?;
    program.attach(target_syscall_entry, 0)?;
    log::info!("attacch the kprobe to syscall_entry ok");

    for _ in 0..64 {
        unsafe {
            libc::getpid();
        }
        time::sleep(time::Duration::from_millis(10)).await;
    }

    let syscall_list: HashMap<_, u32, u32> =
        HashMap::try_from(ebpf.map("SYSCALL_LIST").unwrap())?;
    let mut total = 0u32;
    let mut distinct = 0u32;
    for item in syscall_list.iter() {
        let (key, value) = item?;
        println!("syscall: {key}, count: {value}");
        total = total.saturating_add(value);
        distinct += 1;
    }

    if total == 0 {
        anyhow::bail!("SYSCALL_COUNT_FAIL: no syscall records were captured");
    }

    println!("SYSCALL_COUNT_PASS: {total} records across {distinct} syscall ids");

    Ok(())
}
