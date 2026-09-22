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
static TEST_VM: LazyLock<Mutex<Option<TestVm>>> = LazyLock::new(|| Mutex::new(None));

pub(crate) fn set_vm_status(vm_id: VMId, status: Option<VmStatus>) {
    *TEST_VM.lock() = status.map(|status| TestVm { id: vm_id, status });
}

/// Removes and returns every recorded `notify_vm` target.
///
/// The guest-console tests drain this to observe that consuming an ordered
/// record published a device-poll request for exactly the blocked VM.
pub(crate) fn take_notified_vms() -> Vec<VMId> {
    let mut notified = NOTIFIED_VMS.lock();
    core::mem::take(&mut notified)
}

#[derive(Clone)]
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

    pub(crate) fn vm_by_id(vm_id: VMId) -> Option<TestVm> {
        TEST_VM.lock().as_ref().filter(|vm| vm.id == vm_id).cloned()
    }

    pub(crate) fn vm_list() -> Vec<TestVm> {
        TEST_VM.lock().iter().cloned().collect()
    }
}
