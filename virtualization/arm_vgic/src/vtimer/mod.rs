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

use axdevice_base::Device;

mod cntp_timer;
pub use cntp_timer::CntpTimerState as VtimerState;

mod cntp_cval_el0;
pub use cntp_cval_el0::SysCntpCvalEl0;

mod cntp_ctl_el0;
pub use cntp_ctl_el0::SysCntpCtlEl0;

mod cntpct_el0;
pub use cntpct_el0::SysCntpctEl0;

mod cntp_tval_el0;
pub use cntp_tval_el0::SysCntpTvalEl0;

/// Create the concrete system-register devices backed by per-vCPU timer banks.
pub fn new_sysreg_devices() -> (SysCntpCvalEl0, SysCntpCtlEl0, SysCntpctEl0, SysCntpTvalEl0) {
    let (_timer, cval, ctl, counter, tval) = new_sysreg_devices_with_state();

    (cval, ctl, counter, tval)
}

/// Create the concrete system-register devices and their shared timer state.
pub fn new_sysreg_devices_with_state() -> (
    Arc<VtimerState>,
    SysCntpCvalEl0,
    SysCntpCtlEl0,
    SysCntpctEl0,
    SysCntpTvalEl0,
) {
    let timer = Arc::new(VtimerState::new());

    (
        Arc::clone(&timer),
        SysCntpCvalEl0::from_state(Arc::clone(&timer)),
        SysCntpCtlEl0::from_state(Arc::clone(&timer)),
        SysCntpctEl0::new(),
        SysCntpTvalEl0::from_state(timer),
    )
}

/// Create a collection of system register devices.
pub fn get_sysreg_device() -> Vec<Arc<dyn Device>> {
    let (_timer, devices) = get_sysreg_device_with_state();

    devices
}

/// Create system register devices together with their shared timer state.
pub fn get_sysreg_device_with_state() -> (Arc<VtimerState>, Vec<Arc<dyn Device>>) {
    let (timer, cval, ctl, counter, tval) = new_sysreg_devices_with_state();

    (
        timer,
        vec![
            Arc::new(cval),
            Arc::new(ctl),
            Arc::new(counter),
            Arc::new(tval),
        ],
    )
}

#[cfg(test)]
mod tests {
    use axdevice_base::AccessWidth;

    use super::*;

    #[test]
    fn concrete_devices_isolate_timer_state_per_vcpu() {
        let (timer, cval, ctl, _counter, tval) = new_sysreg_devices_with_state();

        timer.set_test_current_identity(7, 0);
        cval.write_register(AccessWidth::Qword, 0x1111).unwrap();
        ctl.write_register(AccessWidth::Dword, 0x3).unwrap();

        timer.set_test_current_identity(7, 1);
        cval.write_register(AccessWidth::Qword, 0x2222).unwrap();
        ctl.write_register(AccessWidth::Dword, 0x1).unwrap();

        timer.set_test_current_identity(7, 0);
        assert_eq!(cval.read_register(AccessWidth::Qword).unwrap(), 0x1111);
        assert_eq!(tval.read_register(AccessWidth::Dword).unwrap(), 0x1111);
        assert_eq!(ctl.read_register(AccessWidth::Dword).unwrap() & 0x3, 0x3);

        timer.set_test_current_identity(7, 1);
        assert_eq!(cval.read_register(AccessWidth::Qword).unwrap(), 0x2222);
        assert_eq!(tval.read_register(AccessWidth::Dword).unwrap(), 0x2222);
        assert_eq!(ctl.read_register(AccessWidth::Dword).unwrap() & 0x3, 0x1);
    }
}
