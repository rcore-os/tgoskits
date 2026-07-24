use alloc::{
    string::{String, ToString},
    sync::Arc,
};

use ax_kernel_guard::NoPreemptIrqSave;
use ax_runtime::hal::cpu::uspace::UserContext;
use ax_sync::Mutex;
use ax_task::{AxTaskExt, spawn_task};
use starry_process::{Pid, Process};

use crate::{
    file::FD_TABLE,
    mm::{copy_from_kernel, load_user_app, new_user_aspace_empty},
    pseudofs::{self, dev::tty},
    task::{ProcessData, ProcessImage, Thread, add_task_to_table, new_user_task, spawn_alarm_task},
    tracepoint::tracepoint_init,
};

/// Initialize and run initproc.
pub fn init(args: &[String], envs: &[String]) {
    static_keys::global_init();

    tracepoint_init().expect("Failed to initialize tracepoints");

    crate::ebpf::init_ebpf();
    crate::perf::perf_event_init();
    crate::kmod::init_kmod();

    pseudofs::mount_all().expect("Failed to mount pseudofs");
    spawn_alarm_task();
    // DVFS: a one-shot OPP-calibration boot runs the sweep and skips the governor;
    // otherwise start the ondemand governor. Both run here (early init, before the
    // console tty handoff) so their kernel logs reach the serial console.
    if ax_driver::cpufreq::calibrate_wanted() {
        run_opp_calibration();
    } else {
        spawn_cpufreq_governor();
    }
    pseudofs::usbfs::start_event_pump();

    ax_alloc::register_page_reclaim_fn(ax_fs_ng::vfs::page_cache_reclaim);

    let loc = ax_fs_ng::vfs::current_fs_context()
        .lock()
        .resolve(&args[0])
        .expect("Failed to resolve executable path");
    let path = loc
        .absolute_path()
        .expect("Failed to get executable absolute path");
    let name = loc.name().into_owned();

    let mut uspace = new_user_aspace_empty()
        .and_then(|mut it| {
            copy_from_kernel(&mut it)?;
            Ok(it)
        })
        .expect("Failed to create user address space");

    let (entry_vaddr, ustack_top, auxv) = load_user_app(&mut uspace, loc, &args[0], args, envs)
        .unwrap_or_else(|e| panic!("Failed to load user app: {}", e));

    let uctx = UserContext::new(entry_vaddr.into(), ustack_top, 0);
    let mut task = new_user_task(&name, uctx, 0);
    task.ctx_mut().set_page_table_root(uspace.page_table_root());

    // PID 1 must really be 1: the init process is the root of the process
    // hierarchy and userspace (e.g. systemd's `getpid() == 1` system-manager
    // check) relies on it. The scheduler task id is an internal counter that is
    // already past 1 by the time we spawn the user init (kernel helper tasks
    // took the low ids), so we pin the user-visible pid/tid to 1 and leave the
    // scheduler id untouched. `Thread::tid` is already decoupled from the
    // scheduler id (see its field doc), so this only requires the table keys to
    // follow the thread tid rather than `task.id()`.
    const INIT_PID: Pid = 1;
    let pid = INIT_PID;
    let proc = Process::new_init(pid);
    proc.add_thread(pid);

    if let Err(err) = tty::bind_console_to(&proc) {
        warn!("Failed to bind console tty: {err:?}");
    }

    let proc = ProcessData::new(
        proc,
        ProcessImage::new(
            path.to_string(),
            Arc::new(args.to_vec()),
            Arc::new(envs.to_vec()),
            auxv,
            "/".to_string(),
            "/".to_string(),
        ),
        Arc::new(Mutex::new(uspace)),
        Arc::default(),
        None,
        pid,
        false,
    );

    let mut scope = scope_local::Scope::new();
    crate::file::add_stdio(&mut FD_TABLE.scope_mut(&mut scope).write())
        .expect("Failed to add stdio");

    let thr = Thread::new(pid, proc, None, starry_signal::SignalSet::default(), scope);
    *task.task_ext_mut() = Some(AxTaskExt::from_impl(thr));

    let task = {
        let _guard = NoPreemptIrqSave::new();
        let task = spawn_task(task);
        add_task_to_table(&task);
        tty::arm_console_irq();
        task
    };

    // TODO: wait for all processes to finish
    let exit_code = task.join();
    info!("Init process exited with code: {exit_code:?}");

    let fs_context = ax_fs_ng::vfs::current_fs_context();
    let cx = fs_context.lock();
    cx.root_dir()
        .unmount_all()
        .expect("Failed to unmount all filesystems");
    cx.root_dir()
        .filesystem()
        .flush()
        .expect("Failed to flush rootfs");
}

/// Run the one-shot DVFS OPP calibration sweep (gated by the driver's `CALIBRATE`
/// const). Each cluster's (voltage x ring) sweep must execute ON a core of that
/// cluster to read that core's own PMU cycle counter, so we pin a task per cluster
/// (cpu0=A55, cpu4=A76 big0, cpu6=A76 big1) via `set_current_affinity` and run
/// them sequentially (the two A76 rails share one I2C bus). Synchronous: it blocks
/// init briefly so the `CAL` log lines land before the console tty handoff.
fn run_opp_calibration() {
    info!("cpufreq: running OPP calibration sweep (governor disabled this boot)");
    for &(cluster_idx, cpu) in &[(0usize, 0usize), (1, 4), (2, 6)] {
        let task = ax_task::spawn_raw(
            move || {
                ax_task::set_current_affinity(ax_task::AxCpuMask::one_shot(cpu));
                ax_driver::cpufreq::calibrate_cluster(cluster_idx, cpu);
            },
            String::from("cpufreq-cal"),
            ax_task::default_task_stack_size(),
        );
        task.join();
    }
    info!("cpufreq: OPP calibration sweep complete");
}

/// Start the CPU DVFS ondemand governor.
///
/// The frequency/voltage policy and the SCMI+PMIC apply live in the cpufreq
/// driver (`ax_driver::cpufreq`); this kernel task is only the driver's periodic
/// *loop*. The loop must live here, not in the driver, because ax-driver sits
/// below ax-task/ax-hal in the dependency graph (they pull ax-driver back in via
/// axplat-dyn), so spawning a task inside the driver would be a cyclic dep. Each
/// period we snapshot the per-CPU busy counters the scheduler tick maintains and
/// hand them to `governor_poll`, which decides and applies any OPP change.
///
/// No-op unless the driver armed the governor (feature on and both CPU-rail PMIC
/// buses up); otherwise every cluster stays on its boot OPP.
fn spawn_cpufreq_governor() {
    if !ax_driver::cpufreq::governor_wanted() {
        return;
    }
    info!("Initialize cpufreq ondemand governor...");
    ax_task::spawn_raw(
        cpufreq_governor_loop,
        String::from("cpufreq-gov"),
        ax_task::default_task_stack_size(),
    );
}

/// Periodic body of the DVFS governor task: sleep, sample every CPU's cumulative
/// busy-tick counter, and let the driver scale each cluster to match load. The
/// slow work (SCMI SMC + PMIC I2C/SPI voltage ramp) happens inside
/// `governor_poll`, which is why this runs in a sleepable task rather than the
/// scheduler tick.
fn cpufreq_governor_loop() {
    let period = core::time::Duration::from_millis(ax_driver::cpufreq::governor_period_ms());
    loop {
        ax_task::sleep(period);
        // RK3588 has 8 CPUs; an offline core's counter never advances, so it
        // simply reads as idle (conservative — never over-scales).
        let mut busy = [0u64; 8];
        for (cpu, slot) in busy.iter_mut().enumerate() {
            *slot = ax_task::cpu_busy_ticks(cpu);
        }
        ax_driver::cpufreq::governor_poll(&busy);
    }
}
