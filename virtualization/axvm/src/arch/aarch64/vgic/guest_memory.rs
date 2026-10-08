//! Checked AxVM guest-memory adapter used by the software ITS.
//!
//! The adapter carries no numeric VM identity and performs no global registry
//! lookup. It is constructed empty with the run's device graph and bound exactly
//! once, in task context, with that run's [`GuestMemoryPort`]. Every ITS
//! command-queue read then copies through a scoped access lease that retains one
//! mapping revision and its backing, so the read observes the same translation
//! and RAM ownership the run published.
//!
//! The capability is a task-only resource: the ITS command queue is drained from
//! the guest's GITS MMIO write path, which the run executes with the backend
//! unloaded. Hardware entries (`Aarch64Entry`) never hold it, and the native
//! pending/active callbacks that run from host IRQs never reach it.

use std::sync::OnceLock;

use arm_vgic::{GuestMemory, GuestMemoryError};

use crate::{AxVmError, AxVmResult, GuestPhysAddr, guest_memory::GuestMemoryPort};

pub(super) struct AxvmGuestMemory {
    memory: OnceLock<GuestMemoryPort>,
}

impl AxvmGuestMemory {
    pub(super) const fn new() -> Self {
        Self {
            memory: OnceLock::new(),
        }
    }

    /// Binds this run's guest-memory capability exactly once.
    ///
    /// A second binding is a conflict: the adapter belongs to a single execution
    /// period, and a stale run can never re-target a newer adapter.
    pub(super) fn bind(&self, memory: GuestMemoryPort) -> AxVmResult {
        self.memory.set(memory).map_err(|_| {
            AxVmError::resource_conflict(
                "bind AArch64 ITS guest memory",
                "guest memory is already bound for this run",
            )
        })
    }
}

impl GuestMemory for AxvmGuestMemory {
    fn read(&self, address: u64, destination: &mut [u8]) -> Result<(), GuestMemoryError> {
        let address = usize::try_from(address).map_err(|_| {
            GuestMemoryError::new("read ITS table", "guest address does not fit usize")
        })?;
        let memory = self.memory.get().ok_or_else(|| {
            GuestMemoryError::new("read ITS table", "guest memory is not bound for this run")
        })?;
        let address = GuestPhysAddr::from(address);
        match memory.with_access(|access| access.read(address, destination)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(GuestMemoryError::new(
                "read ITS table",
                std::format!("{error:?}"),
            )),
            Err(error) => Err(GuestMemoryError::new(
                "read ITS table",
                std::format!("{error:?}"),
            )),
        }
    }
}
