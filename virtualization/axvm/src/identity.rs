//! Instance, execution and operation identities issued by their owners.

use crate::{AxVmError, AxVmResult, VMId};

/// Identifies one VM instance even when the numeric VM ID is reused.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct VmKey {
    vm_id: VMId,
    generation: u64,
}

impl VmKey {
    pub(crate) const fn new(vm_id: VMId, generation: u64) -> Self {
        Self { vm_id, generation }
    }

    /// Returns the numeric identifier used by configuration and the guest ABI.
    pub const fn vm_id(self) -> VMId {
        self.vm_id
    }

    /// Returns the manager-issued instance generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// Identifies one execution period of one VM instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RunId {
    vm: VmKey,
    generation: u64,
}

impl RunId {
    pub(crate) const fn new(vm: VmKey, generation: u64) -> Self {
        Self { vm, generation }
    }

    /// Returns the owning VM instance.
    pub const fn vm(self) -> VmKey {
        self.vm
    }

    /// Returns this instance's execution generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// Identifies an operation within a VM instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct OperationId {
    vm: VmKey,
    sequence: u64,
}

impl OperationId {
    pub(crate) const fn new(vm: VmKey, sequence: u64) -> Self {
        Self { vm, sequence }
    }

    /// Returns the instance whose owner accepts this operation.
    pub const fn vm(self) -> VmKey {
        self.vm
    }

    /// Returns the operation sequence in that instance.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }
}

pub(crate) fn next_generation(counter: &mut u64, resource: &'static str) -> AxVmResult<u64> {
    let next = counter.checked_add(1).ok_or_else(|| {
        AxVmError::resource_unavailable(resource, "identity generation exhausted")
    })?;
    *counter = next;
    Ok(next)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct VcpuInstance {
    pub run: RunId,
    pub vcpu_id: usize,
    pub activation: u64,
}
