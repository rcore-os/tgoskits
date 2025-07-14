extern crate alloc;

use alloc::sync::Arc;
use alloc::{vec, vec::Vec};
use axdevice_base::BaseSysRegDeviceOps;

mod cntp_ctl_el0;
pub use cntp_ctl_el0::SysCntpCtlEl0;

mod cntpct_el0;
pub use cntpct_el0::SysCntpctEl0;

mod cntp_tval_el0;
pub use cntp_tval_el0::SysCntpTvalEl0;

/// Create a collection of system register devices.
pub fn get_sysreg_device() -> Vec<Arc<dyn BaseSysRegDeviceOps>> {
    vec![
        Arc::new(SysCntpCtlEl0::new()),
        Arc::new(SysCntpctEl0::new()),
        Arc::new(SysCntpTvalEl0::new()),
    ]
}
