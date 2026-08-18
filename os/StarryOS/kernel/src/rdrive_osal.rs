//! StarryOS integration of rdrive's OS-abstraction layer ([`rdrive::Osal`]).
//!
//! rdrive tags every device lock with the pid of the process that acquired it,
//! and can free every lock still attributed to a dead process
//! ([`rdrive::reclaim_all_held_by`], invoked from the process-exit path). Both
//! rely on `Osal::get_pid()` reporting the *current process* pid; rdrive's
//! default `Osal` always returns [`Pid::INVALID`], so without this adapter no
//! lock is attributable to a process and reclaim can never match anything.
//!
//! This adapter lives in the StarryOS layer (not the ArceOS `axruntime` layer)
//! because it needs StarryOS process/task context, which only exists above
//! ArceOS.

use ax_task::current;
use rdrive::{Osal, Pid};

use crate::task::AsThread;

/// StarryOS [`Osal`] adapter. Zero-sized; a single `'static` instance is
/// installed with rdrive once at kernel init.
pub struct StarryOsal;

/// The single adapter instance registered with rdrive.
pub static STARRY_OSAL: StarryOsal = StarryOsal;

impl Osal for StarryOsal {
    /// The root-namespace thread-group id (pid) of the process running the
    /// current task.
    ///
    /// Kernel tasks (early init, IRQ context, kernel worker threads) carry no
    /// thread extension and therefore no process; they report [`Pid::INVALID`]
    /// so any device lock they take stays "held but untracked" and is never
    /// mis-reclaimed as belonging to some numeric pid.
    fn get_pid(&self) -> Pid {
        match current().try_as_thread() {
            Some(thread) => (thread.proc_data.proc.pid().get() as usize).into(),
            None => Pid::INVALID.into(),
        }
    }

    /// Yield to the scheduler while blocked on a contended device lock, instead
    /// of hard-spinning a CPU the current owner may need to make progress. The
    /// rdrive `lock()` path is already documented as not hard-IRQ safe, so a
    /// sleepable yield is the right backoff here.
    fn relax(&self) {
        ax_task::yield_now();
    }
}

/// Installs the StarryOS [`Osal`] with rdrive.
///
/// Must run before any process acquires a device lock, so it is called at the
/// very start of StarryOS kernel init. Installing a static is idempotent, so a
/// re-install (e.g. from a test that briefly swaps the `Osal`) is harmless.
pub fn init() {
    rdrive::set_osal(&STARRY_OSAL);
}

/// End-to-end regression for the process-exit device-lock reclaim path.
///
/// Drives the exact public API `do_exit` uses ([`rdrive::reclaim_all_held_by`])
/// against a real entry in the global registry, with the lock owner set through
/// the installed [`Osal`] — not the private device container. A fixed-pid
/// `Osal` stands in for "a process that acquired a device lock and then died
/// without releasing it"; reclaim of that pid must free the lock so the next
/// acquirer is no longer blocked.
#[cfg(axtest)]
pub(crate) fn exit_reclaim_frees_dead_holder_device_for_test() -> bool {
    use rdrive::driver::Empty;

    /// A distinctive pid, far above any pid StarryOS hands out in an axtest run,
    /// so the reclaim below cannot alias a live process's locks.
    const DEAD_PID: u32 = 0x00AD_DEAD;

    /// Fixed-pid `Osal` standing in for the dead lock-holder. The lock owner is
    /// set through `get_pid()`, exactly as a real acquire on that process's
    /// behalf would set it.
    struct DeadHolderOsal;
    impl Osal for DeadHolderOsal {
        fn get_pid(&self) -> Pid {
            (DEAD_PID as usize).into()
        }
    }
    static DEAD_OSAL: DeadHolderOsal = DeadHolderOsal;

    // The reclaim path requires the global registry; without it there is
    // nothing to reclaim from and the test cannot run meaningfully.
    if !rdrive::is_initialized() {
        return false;
    }

    // Publish a fresh device in the *global* registry so we exercise the real
    // public reclaim entry point, not the private device container.
    let id = rdrive::test_register_empty_device();
    let Ok(device) = rdrive::get::<Empty>(id) else {
        return false;
    };

    // The dead process acquires the lock (owner = DEAD_PID, via get_pid) and
    // leaks the guard: it dies without ever releasing. Restore the real Osal
    // immediately afterwards — the rest of the flow does not depend on the
    // injected pid.
    rdrive::set_osal(&DEAD_OSAL);
    let acquired = device.lock();
    rdrive::set_osal(&STARRY_OSAL);
    let held = match acquired {
        Ok(guard) => guard,
        Err(_) => return false,
    };
    core::mem::forget(held);

    // While the dead holder's lock is outstanding, a fresh acquire is refused.
    let blocked_before = device.try_lock().is_err();

    // The process-exit reclaim: free every device lock held by the dead pid.
    let reclaimed = rdrive::reclaim_all_held_by(DEAD_PID);

    // After reclaim the device is acquirable again (was UsedByOthers, now free).
    // The guard from this probe drops at the end of the expression, releasing it.
    let relockable = device.try_lock().is_ok();

    blocked_before && reclaimed == 1 && relockable
}
