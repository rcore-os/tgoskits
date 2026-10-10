//! Minimal instance-owned VM-manager surface required by the guest-console
//! axtest harness.
//!
//! The harness compiles the production guest-console mux, which reads only the
//! application manager facade: it resolves a handle with `get`, walks `list`,
//! and asks the manager to wake the device poller with `notify_vm`. This stub
//! mirrors those three calls over mutable test state instead of a real VmManager.

#![allow(
    dead_code,
    reason = "the production mux requires the complete manager surface at compile time"
)]

use alloc::{string::String, vec::Vec};
use std::sync::LazyLock;

use anyhow::Result;
use ax_std::sync::Mutex;
use axvm::{VMId, VmStatus};

/// VM IDs the production mux asked the manager to wake, in call order.
static NOTIFIED_VMS: LazyLock<Mutex<Vec<VMId>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static TEST_VM: LazyLock<Mutex<Option<TestVm>>> = LazyLock::new(|| Mutex::new(None));

pub(crate) fn set_vm_status(vm_id: VMId, status: Option<VmStatus>) {
    *TEST_VM.lock() = status.map(|state| TestVm {
        key: TestKey { vm_id },
        snapshot: TestSnapshot {
            name: String::new(),
            state,
        },
    });
}

/// Removes and returns every recorded `notify_vm` target.
///
/// The guest-console tests drain this to observe that consuming an ordered
/// record published a device-poll request for exactly the blocked VM.
pub(crate) fn take_notified_vms() -> Vec<VMId> {
    let mut notified = NOTIFIED_VMS.lock();
    core::mem::take(&mut notified)
}

/// Instance identity stub mirroring `axvm::VmKey`.
#[derive(Clone)]
pub(crate) struct TestKey {
    vm_id: VMId,
}

impl TestKey {
    pub(crate) fn vm_id(&self) -> VMId {
        self.vm_id
    }
}

/// Immutable observation stub mirroring `axvm::VmSnapshot`.
#[derive(Clone)]
pub(crate) struct TestSnapshot {
    #[allow(
        dead_code,
        reason = "the network console layout reads the configured name"
    )]
    pub(crate) name: String,
    pub(crate) state: VmStatus,
}

/// Handle stub mirroring `axvm::VmHandle`.
#[derive(Clone)]
pub(crate) struct TestVm {
    key: TestKey,
    snapshot: TestSnapshot,
}

impl TestVm {
    pub(crate) fn key(&self) -> TestKey {
        self.key.clone()
    }

    pub(crate) fn snapshot(&self) -> TestSnapshot {
        self.snapshot.clone()
    }
}

/// Manager stub mirroring the production application manager.
pub(crate) struct TestManager;

impl TestManager {
    pub(crate) fn notify_vm(&self, vm_id: VMId) -> Result<()> {
        NOTIFIED_VMS.lock().push(vm_id);
        Ok(())
    }

    pub(crate) fn get(&self, vm_id: VMId) -> Option<TestVm> {
        TEST_VM
            .lock()
            .as_ref()
            .filter(|vm| vm.key.vm_id == vm_id)
            .cloned()
    }

    pub(crate) fn list(&self) -> Vec<TestVm> {
        TEST_VM.lock().iter().cloned().collect()
    }
}

static TEST_MANAGER: TestManager = TestManager;

pub(crate) fn manager() -> &'static TestManager {
    &TEST_MANAGER
}
