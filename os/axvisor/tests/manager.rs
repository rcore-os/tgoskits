//! Minimal VM-manager surface required by the guest-console axtest harness.

#![allow(
    dead_code,
    reason = "the production mux requires the complete manager surface at compile time"
)]

use alloc::vec::Vec;

use anyhow::Result;
use axvm::{VMId, VmStatus};

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
    pub(crate) fn notify_vm(_vm_id: VMId) -> Result<()> {
        Ok(())
    }

    pub(crate) fn vm_by_id(_vm_id: VMId) -> Option<TestVm> {
        None
    }

    pub(crate) fn vm_list() -> Vec<TestVm> {
        Vec::new()
    }
}
