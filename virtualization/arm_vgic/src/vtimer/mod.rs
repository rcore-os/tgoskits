// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

extern crate alloc;

use alloc::{sync::Arc, vec, vec::Vec};

use axdevice_base::BaseSysRegDeviceOps;

mod cntp_timer;

mod cntp_cval_el0;
pub use cntp_cval_el0::SysCntpCvalEl0;

mod cntp_ctl_el0;
pub use cntp_ctl_el0::SysCntpCtlEl0;

mod cntpct_el0;
pub use cntpct_el0::SysCntpctEl0;

mod cntp_tval_el0;
pub use cntp_tval_el0::SysCntpTvalEl0;

/// Create the concrete system-register devices backed by one timer state.
pub fn new_sysreg_devices() -> (SysCntpCvalEl0, SysCntpCtlEl0, SysCntpctEl0, SysCntpTvalEl0) {
    let timer = Arc::new(cntp_timer::CntpTimerState::new());

    (
        SysCntpCvalEl0::from_state(Arc::clone(&timer)),
        SysCntpCtlEl0::from_state(Arc::clone(&timer)),
        SysCntpctEl0::new(),
        SysCntpTvalEl0::from_state(timer),
    )
}

/// Create a collection of system register devices.
pub fn get_sysreg_device() -> Vec<Arc<dyn BaseSysRegDeviceOps>> {
    let (cval, ctl, counter, tval) = new_sysreg_devices();

    vec![
        Arc::new(cval),
        Arc::new(ctl),
        Arc::new(counter),
        Arc::new(tval),
    ]
}

#[cfg(test)]
mod tests {
    use aarch64_sysreg::SystemRegType;
    use axdevice_base::{AccessWidth, BaseDeviceOps, SysRegAddr};

    use super::*;

    #[test]
    fn concrete_devices_share_timer_state() {
        let (cval, _ctl, _counter, tval) = new_sysreg_devices();
        let cval_addr = SysRegAddr::new(SystemRegType::CNTP_CVAL_EL0 as usize);
        let tval_addr = SysRegAddr::new(SystemRegType::CNTP_TVAL_EL0 as usize);
        let value = 0x1234_5678usize;

        cval.handle_write(cval_addr, AccessWidth::Qword, value)
            .unwrap();

        assert_eq!(
            tval.handle_read(tval_addr, AccessWidth::Dword).unwrap(),
            value
        );
    }
}
