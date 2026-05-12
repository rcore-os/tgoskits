use rdif_base::custom_type;
use spin::RwLock;

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
    /// Get the current process ID.
    fn get_pid(&self) -> Pid;
}

struct OsalImplEmplty;

impl Osal for OsalImplEmplty {
    fn get_pid(&self) -> Pid {
        Pid::INVALID.into()
    }
}

static OSAL: RwLock<&dyn Osal> = RwLock::new(&OsalImplEmplty);

pub fn set_osal(osal: &'static dyn Osal) {
    let mut guard = OSAL.write();
    *guard = osal;
}

pub(crate) fn get_pid() -> Pid {
    OSAL.read().get_pid()
}
