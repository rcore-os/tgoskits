use ax_sync::SpinRwLock as RwLock;
use rdif_base::custom_type;

custom_type!(#[doc="Process ID"],Pid, usize, "{:?}");

impl Pid {
    pub const NOT_SET: usize = -1isize as usize;
    pub const INVALID: usize = -2isize as usize;

    pub fn is_not_set(&self) -> bool {
        self.0 == Pid::NOT_SET
    }

    pub fn is_invalid(&self) -> bool {
        self.0 == Pid::INVALID
    }
}

pub trait Osal: Sync + Send + 'static {
    /// Get a diagnostic process label. It does not own or revoke device borrows.
    fn get_pid(&self) -> Pid;

    /// Called on every iteration of a contended blocking `lock()`.
    ///
    /// The default just hints the CPU that this is a spin-wait. An OS
    /// integration may yield the current task when blocking device acquisition
    /// is restricted to sleepable task context. This callback runs without the
    /// OSAL registration lock held.
    fn relax(&self) {
        core::hint::spin_loop();
    }
}

struct DefaultOsal;

impl Osal for DefaultOsal {
    fn get_pid(&self) -> Pid {
        Pid::INVALID.into()
    }
}

struct OsalSlot(RwLock<&'static dyn Osal>);

impl OsalSlot {
    const fn new(osal: &'static dyn Osal) -> Self {
        Self(RwLock::new(osal))
    }

    fn get_pid(&self) -> Pid {
        let osal = *self.0.read();
        osal.get_pid()
    }

    fn relax(&self) {
        // Copy the static adapter before invoking arbitrary OS code, which
        // may schedule or register an adapter itself.
        let osal = *self.0.read();
        osal.relax();
    }
}

static OSAL: OsalSlot = OsalSlot::new(&DefaultOsal);

/// Install an OS adapter. In-flight callbacks may finish on the old adapter.
pub fn set_osal(osal: &'static dyn Osal) {
    *OSAL.0.write() = osal;
}

pub(crate) fn get_pid() -> Pid {
    OSAL.get_pid()
}

pub(crate) fn relax() {
    OSAL.relax();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callbacks_run_outside_osal_lock() {
        struct Probe;
        static SLOT: OsalSlot = OsalSlot::new(&Probe);
        impl Osal for Probe {
            fn get_pid(&self) -> Pid {
                assert!(SLOT.0.try_write().is_some(), "get_pid holds OSAL lock");
                Pid::INVALID.into()
            }
            fn relax(&self) {
                assert!(SLOT.0.try_write().is_some(), "relax holds OSAL lock");
            }
        }
        SLOT.get_pid();
        SLOT.relax();
    }
}
