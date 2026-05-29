use aya::programs::UProbe;
use clap::Parser;
#[rustfmt::skip]
use log::{debug, warn};
use tokio::signal;

#[derive(Debug, Parser)]
struct Opt {
    #[clap(short, long)]
    pid: Option<i32>,
}

#[inline(never)]
#[unsafe(no_mangle)]
fn uprobe_test(a: u32, b: Option<u32>) -> u32 {
    println!("In uprobe_test: a = {}, b = {:?}", a, b);
    a + b.unwrap_or(0)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _opt = Opt::parse();

    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
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
        "/upb"
    )))?;

    tokio::task::spawn(async move {
        // let start = tokio::time::Instant::now();
        loop {
            // call the uprobe_test function to trigger the uprobe
            uprobe_test(42, Some(58));
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            // let elapsed = start.elapsed().as_secs();
            // println!("eBPF program has been running for {elapsed} seconds");
        }
    });

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
    // let Opt { pid } = opt;
    let program: &mut UProbe = ebpf.program_mut("upb").unwrap().try_into()?;
    program.load()?;

    let pid = std::process::id() as i32;
    println!("Attaching to process with PID: {}", pid); 
    println!("Attaching uprobe to function 'uprobe_test': {:#x} in current process", uprobe_test as *const () as usize);
    // program.attach("getaddrinfo", "libc", pid, None /* cookie */)?;
    program.attach("uprobe_test", "upb", Some(pid), None /* cookie */)?;

    let ctrl_c = signal::ctrl_c();
    println!("Waiting for Ctrl-C...");
    ctrl_c.await?;
    println!("Exiting...");

    Ok(())
}
