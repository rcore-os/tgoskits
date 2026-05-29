use aya::{maps::HashMap, programs::KProbe};
#[rustfmt::skip]
use log::{debug, warn};
use tokio::{signal, task::yield_now, time};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Warn)
        .format_timestamp(None)
        .init();

    let args_num = std::env::args().len();
    if args_num != 2 {
        println!("Usage: syscall_ebpf {{mangled_func_name}}");
        println!(
            "You can get the mangled function name by running `cat /proc/kallsyms | grep -m 1 sysno`"
        );
        return Ok(());
    }
    let args: Vec<String> = std::env::args().collect();
    let target_syscall_entry = &args[1];

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
        "/syscall_ebpf"
    )))?;
    match aya_log::EbpfLogger::init(&mut ebpf) {
        Err(e) => {
            // This can happen if you remove all log statements from your eBPF program.
            warn!("failed to initialize eBPF logger: {e}");
        }
        Ok(logger) => {
            let mut logger =
                tokio::io::unix::AsyncFd::with_interest(logger, tokio::io::Interest::READABLE)?;
            tokio::task::spawn(async move {
                loop {
                    let mut guard = logger.readable_mut().await.unwrap();
                    guard.get_inner_mut().flush();
                    guard.clear_ready();
                }
            });
        }
    }
    let program: &mut KProbe = ebpf.program_mut("syscall_ebpf").unwrap().try_into()?;
    program.load()?;
    program.attach(target_syscall_entry, 0)?;
    log::info!("attacch the kprobe to syscall_entry ok");

    // print the value of the blocklist per 5 seconds
    tokio::spawn(async move {
        let blocklist: HashMap<_, u32, u32> =
            HashMap::try_from(ebpf.map("SYSCALL_LIST").unwrap()).unwrap();
        let mut now = time::Instant::now();
        loop {
            let new_now = time::Instant::now();
            let duration = new_now.duration_since(now);
            if duration.as_secs() >= 5 {
                println!("------------SYSCALL_LIST----------------");
                let iter = blocklist.iter();
                for item in iter {
                    if let Ok((key, value)) = item {
                        println!("syscall: {:?}, count: {:?}", key, value);
                    }
                }
                println!("----------------------------------------");
                now = new_now;
            }
            yield_now().await;
        }
    });

    let ctrl_c = signal::ctrl_c();
    println!("Waiting for Ctrl-C...");
    ctrl_c.await?;
    println!("Exiting...");

    Ok(())
}
