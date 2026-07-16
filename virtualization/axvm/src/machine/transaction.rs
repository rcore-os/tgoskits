//! Transactional ownership transfer for planned host devices.

use alloc::{boxed::Box, collections::BTreeMap, format, string::ToString, vec::Vec};

use spin::Mutex;

use super::{HostDeviceId, MachinePlanError, MachinePlanResult, VmMachinePlan};

/// One claimed host device whose destructor restores the complete host state.
///
/// Implementations own any saved IRQ route, priority, trigger, DMA, power, and
/// driver state needed to return the device to the host. Dropping the lease is
/// the only rollback operation used by the generic transaction.
pub trait HostDeviceLease: Send {}

/// Platform capability used to atomically claim the physical devices in a VM
/// machine plan.
pub trait HostDeviceClaimProvider {
    /// Returns the current generation of the live platform inventory.
    fn snapshot_generation(&self) -> u64;

    /// Claims one device and returns a lease that restores it when dropped.
    fn claim(&self, device: &HostDeviceId) -> MachinePlanResult<Box<dyn HostDeviceLease>>;
}

static HOST_DEVICE_OWNERS: Mutex<BTreeMap<HostDeviceId, usize>> = Mutex::new(BTreeMap::new());

/// VM-level claim provider that serializes planned host-device ownership.
///
/// This registry closes the race between independent VM build transactions.
/// Architecture and device adapters remain responsible for acquiring their
/// concrete IRQ, DMA, power, and driver-state capabilities inside the lease
/// lifetime established here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegisteredHostDeviceClaimProvider {
    snapshot_generation: u64,
    vm_id: usize,
}

impl RegisteredHostDeviceClaimProvider {
    /// Creates a provider for one live snapshot and VM owner.
    pub const fn new(snapshot_generation: u64, vm_id: usize) -> Self {
        Self {
            snapshot_generation,
            vm_id,
        }
    }
}

impl HostDeviceClaimProvider for RegisteredHostDeviceClaimProvider {
    fn snapshot_generation(&self) -> u64 {
        self.snapshot_generation
    }

    fn claim(&self, device: &HostDeviceId) -> MachinePlanResult<Box<dyn HostDeviceLease>> {
        let mut owners = HOST_DEVICE_OWNERS.lock();
        if let Some(owner) = owners.get(device) {
            return Err(MachinePlanError::ClaimRejected {
                device: device.to_string(),
                detail: format!("already owned by VM {owner}"),
            });
        }
        owners.insert(device.clone(), self.vm_id);
        Ok(Box::new(RegisteredHostDeviceLease {
            device: device.clone(),
            vm_id: self.vm_id,
        }))
    }
}

struct RegisteredHostDeviceLease {
    device: HostDeviceId,
    vm_id: usize,
}

impl HostDeviceLease for RegisteredHostDeviceLease {}

impl Drop for RegisteredHostDeviceLease {
    fn drop(&mut self) {
        let mut owners = HOST_DEVICE_OWNERS.lock();
        if owners.get(&self.device) == Some(&self.vm_id) {
            owners.remove(&self.device);
        }
    }
}

/// In-progress host-device ownership transaction.
///
/// If construction or any later VM build stage fails, dropping this value
/// releases already claimed devices in reverse acquisition order.
pub struct VmMachineTransaction {
    leases: Option<Vec<Box<dyn HostDeviceLease>>>,
}

impl VmMachineTransaction {
    /// Revalidates the snapshot generation and claims every planned device.
    pub fn claim(
        plan: &VmMachinePlan,
        provider: &dyn HostDeviceClaimProvider,
    ) -> MachinePlanResult<Self> {
        let current = provider.snapshot_generation();
        if current != plan.snapshot_generation() {
            return Err(MachinePlanError::SnapshotGenerationChanged {
                planned: plan.snapshot_generation(),
                current,
            });
        }

        let mut leases = Vec::with_capacity(plan.claims().len());
        for device in plan.claims() {
            leases.push(provider.claim(device)?);
        }
        Ok(Self {
            leases: Some(leases),
        })
    }

    /// Converts the transaction into VM-owned leases after all build stages
    /// have succeeded.
    pub fn commit(mut self) -> HostDeviceLeases {
        HostDeviceLeases {
            leases: self.leases.take().unwrap_or_default(),
        }
    }

    /// Returns the number of devices already claimed by this transaction.
    pub fn len(&self) -> usize {
        self.leases.as_ref().map_or(0, Vec::len)
    }

    /// Returns whether this transaction owns no host devices.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl core::fmt::Debug for VmMachineTransaction {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("VmMachineTransaction")
            .field("lease_count", &self.len())
            .finish()
    }
}

impl Drop for VmMachineTransaction {
    fn drop(&mut self) {
        if let Some(leases) = &mut self.leases {
            while leases.pop().is_some() {}
        }
    }
}

/// Host-device leases retained for the complete lifetime of a committed VM.
pub struct HostDeviceLeases {
    leases: Vec<Box<dyn HostDeviceLease>>,
}

impl HostDeviceLeases {
    /// Returns how many physical devices are owned by the VM.
    pub fn len(&self) -> usize {
        self.leases.len()
    }

    /// Returns whether the VM owns no physical devices.
    pub fn is_empty(&self) -> bool {
        self.leases.is_empty()
    }
}

impl core::fmt::Debug for HostDeviceLeases {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("HostDeviceLeases")
            .field("lease_count", &self.leases.len())
            .finish()
    }
}

impl Drop for HostDeviceLeases {
    fn drop(&mut self) {
        while self.leases.pop().is_some() {}
    }
}
