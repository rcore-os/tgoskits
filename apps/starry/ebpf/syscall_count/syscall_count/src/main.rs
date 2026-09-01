use aya::{maps::HashMap, programs::TracePoint};
use log::debug;
use tokio::time;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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

    // This will include your eBPF object file as raw bytes at compile-time and load it at
    // runtime. This approach is recommended for most real-world use cases. If you would
    // like to specify the eBPF program at runtime rather than at compile-time, you can
    // reach for `Bpf::load_file` instead.
    let mut ebpf = aya::Ebpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/syscall_count"
    )))?;

    let program: &mut TracePoint = ebpf.program_mut("syscall_ebpf").unwrap().try_into()?;
    program.load()?;
    program.attach("raw_syscalls", "sys_enter")?;
    log::info!("attached raw_syscalls:sys_enter tracepoint");

    for _ in 0..64 {
        unsafe {
            libc::getpid();
        }
        time::sleep(time::Duration::from_millis(10)).await;
    }

    let syscall_list: HashMap<_, u32, u32> = HashMap::try_from(ebpf.map("SYSCALL_LIST").unwrap())?;
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
