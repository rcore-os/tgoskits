use alloc::{
    string::{String, ToString},
    sync::Arc,
};

use ax_fs_ng::vfs::current_fs_context;
use ax_runtime::hal::cpu::uspace::UserContext;

use crate::{
    file::{FD_TABLE, FileTable},
    mm::{copy_from_kernel, load_user_app, new_user_aspace_empty},
    namespace::NsProxy,
    pseudofs::{self, dev::tty},
    sync::{PiMutex, RwLock},
    task::{
        PidReservation, PidReservationKind, Process, ProcessData, ProcessDataInit, ProcessImage,
        ROOT_PID_NS, Tgid, Thread, Tid, TidNumber, join_kernel_thread, new_user_task,
        prepare_user_thread, sleep, spawn_alarm_task, spawn_kernel_thread,
        spawn_kernel_thread_with_affinity,
    },
    tracepoint::tracepoint_init,
};

/// Initialize and run initproc.
pub fn init(args: &[String], envs: &[String]) {
    crate::stop_machine::init();
    static_keys::global_init();
    crate::cgroup::init();

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

    let loc = current_fs_context()
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

    // PID 1 must really be 1: the init process is the root of the process
    // hierarchy and userspace (e.g. systemd's `getpid() == 1` system-manager
    // check) relies on it. The scheduler task id is an internal counter that is
    // already past 1 by the time we spawn the user init (kernel helper tasks
    // took the low ids), so we pin the user-visible pid/tid to 1 and leave the
    // scheduler id untouched. `Thread::tid` is already decoupled from the
    // scheduler id (see its field doc), so this only requires the table keys to
    // follow the thread tid rather than `task.id()`.
    const INIT_PID: u32 = 1;
    let reservation = PidReservation::reserve(&ROOT_PID_NS, PidReservationKind::ProcessLeader)
        .expect("failed to reserve init PID identity");
    let pid = reservation
        .number_in(&ROOT_PID_NS)
        .expect("init PID reservation has no root binding")
        .get();
    assert_eq!(pid, INIT_PID);
    let identity = reservation.identity();
    let tid_lease = identity
        .acquire_role::<Tid>()
        .expect("failed to acquire init TID role");
    let tgid_lease = identity
        .acquire_role::<Tgid>()
        .expect("failed to acquire init TGID role");
    let proc = Process::new_init(identity.clone());
    proc.add_thread(TidNumber::try_from(pid).expect("init TID must be non-zero"));

    if let Err(error) = tty::bind_console_to(&proc) {
        warn!("Failed to bind console tty: {error:?}");
    }

    let proc = ProcessData::new(
        proc,
        identity.clone(),
        tgid_lease,
        ProcessDataInit::new(
            ProcessImage::new(
                path.to_string(),
                Arc::new(args.to_vec()),
                Arc::new(envs.to_vec()),
                auxv,
                "/".to_string(),
                "/".to_string(),
            ),
            Arc::new(PiMutex::new(uspace)),
            Arc::default(),
            NsProxy::new_root(),
            None,
            TidNumber::try_from(pid).expect("init TID must be non-zero"),
        ),
    );
    // SAFE-EXPECT: failing to attach init would violate the kernel's process accounting invariant.
    crate::cgroup::attach_initial_process(&identity)
        .expect("Failed to attach init process to cgroup root");

    let mut scope = scope_local::Scope::new();
    let mut fd_table = FileTable::new();
    crate::file::add_stdio(&mut fd_table).expect("Failed to add stdio");
    *FD_TABLE.scope_mut(&mut scope) = Arc::new(RwLock::new(fd_table));

    let thr = Thread::new(
        identity.clone(),
        tid_lease,
        proc,
        None,
        starry_signal::SignalSet::default(),
        scope,
    );
    let prepared_task = prepare_user_thread(
        new_user_task(
            uctx,
            0,
            TidNumber::try_from(pid).expect("init TID must be non-zero"),
        ),
        name,
        crate::config::KERNEL_STACK_SIZE,
        thr,
    )
    .expect("failed to prepare init task");
    let staged_task = prepared_task.stage().expect("failed to stage init task");
    let published_identity = reservation
        .publish()
        .expect("failed to publish init PID identity");
    debug_assert!(Arc::ptr_eq(&published_identity, &identity));
    staged_task.with_task(|task| task.as_thread().attach_pid_task(task));
    tty::arm_console_irq();
    let task = staged_task.activate();

    // TODO: wait for all processes to finish
    let exit_code = task.join();
    info!("Init process exited with code: {exit_code:?}");

    let fs_context = current_fs_context();
    let cx = fs_context.lock();
    // Best-effort teardown, matching Linux's shutdown path. A process that exited while
    // holding a mount namespace (bind mounts, pivot_root) can leave the mount tree in a
    // state `unmount_all` rejects; at shutdown that must be logged, not turned into a
    // kernel panic that fails an otherwise clean run. The rootfs flush below is what
    // matters for on-disk integrity.
    if let Err(err) = cx.root_dir().unmount_all() {
        warn!("shutdown: unmount_all failed (best-effort): {err:?}");
    }
    cx.root_dir()
        .filesystem()
        .flush()
        .expect("Failed to flush rootfs");
}

/// Run the one-shot DVFS OPP calibration sweep (gated by the driver's `CALIBRATE`
/// const). Each cluster's (voltage x ring) sweep must execute ON a core of that
/// cluster to read that core's own PMU cycle counter, so we pin a task per cluster
/// (cpu0=A55, cpu4=A76 big0, cpu6=A76 big1) before run-queue publication and run
/// them sequentially (the two A76 rails share one I2C bus). Synchronous: it
/// blocks init briefly so the `CAL` log lines land before the console tty handoff.
fn run_opp_calibration() {
    info!("cpufreq: running OPP calibration sweep (governor disabled this boot)");
    for &(cluster_idx, cpu) in &[(0usize, 0usize), (1, 4), (2, 6)] {
        let mut affinity = ax_runtime::task::CpuSet::empty(ax_runtime::hal::cpu_num());
        let cpu_id =
            u32::try_from(cpu).unwrap_or_else(|_| panic!("cpufreq CPU id {cpu} is out of range"));
        assert!(
            affinity.insert(ax_runtime::task::CpuId::new(cpu_id)),
            "cpufreq calibration CPU {cpu} is outside the runtime topology"
        );
        let task = spawn_kernel_thread_with_affinity(
            move || ax_driver::cpufreq::calibrate_cluster(cluster_idx, cpu),
            String::from("cpufreq-cal"),
            affinity,
        );
        let _exit_code = join_kernel_thread(task);
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
    let _ = spawn_kernel_thread(cpufreq_governor_loop, String::from("cpufreq-gov"));
}

/// Periodic body of the DVFS governor task: sleep, sample every CPU's cumulative
/// non-idle runtime, and let the driver scale each cluster to match load. The
/// slow work (SCMI SMC + PMIC I2C/SPI voltage ramp) happens inside
/// `governor_poll`, which is why this runs in a sleepable task rather than the
/// scheduler tick.
fn cpufreq_governor_loop() {
    let period = core::time::Duration::from_millis(ax_driver::cpufreq::governor_period_ms());
    loop {
        sleep(period);
        // RK3588 has 8 CPUs; an offline or topology-excluded core contributes
        // zero runtime and therefore reads as idle.
        let mut busy = [0u64; 8];
        for (cpu, slot) in busy.iter_mut().enumerate() {
            *slot = ax_runtime::task::cpu_busy_runtime_ns(ax_runtime::task::CpuId::new(cpu as u32))
                .unwrap_or(0);
        }
        ax_driver::cpufreq::governor_poll(&busy);
    }
}
