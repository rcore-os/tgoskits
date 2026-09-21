//! Minimal VM-manager surface required by the guest-console axtest harness.

#![allow(
    dead_code,
    reason = "the production mux requires the complete manager surface at compile time"
)]

use alloc::vec::Vec;
use std::sync::LazyLock;

use anyhow::Result;
use ax_std::sync::Mutex;
use axvm::{VMId, VmStatus};

/// VM IDs the production mux asked the manager to wake, in call order.
static NOTIFIED_VMS: LazyLock<Mutex<Vec<VMId>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Removes and returns every recorded `notify_vm` target.
///
/// The guest-console tests drain this to observe that consuming an ordered
/// record published a device-poll request for exactly the blocked VM.
pub(crate) fn take_notified_vms() -> Vec<VMId> {
    let mut notified = NOTIFIED_VMS.lock();
    core::mem::take(&mut notified)
}

pub(crate) struct TestVm {
    id: VMId,
    status: VmStatus,
}

impl TestVm {
    pub(crate) fn id(&self) -> VMId {
        self.id
    }

    pub(crate) fn status(&self) -> VmStatus {
        self.status
    }
}

pub(crate) struct AxvmManager;

impl AxvmManager {
    pub(crate) fn notify_vm(vm_id: VMId) -> Result<()> {
        NOTIFIED_VMS.lock().push(vm_id);
        Ok(())
    }

    pub(crate) fn vm_by_id(_vm_id: VMId) -> Option<TestVm> {
        None
    }

    pub(crate) fn vm_list() -> Vec<TestVm> {
        Vec::new()
    }
}
