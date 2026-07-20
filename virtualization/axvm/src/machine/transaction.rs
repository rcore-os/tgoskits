//! Transactional ownership transfer for planned host devices.

use alloc::{boxed::Box, collections::BTreeMap, format, string::ToString, vec::Vec};

use ax_kspin::SpinNoIrq as Mutex;

use super::{
    HostDeviceId, HostProviderReference, HostProviderResourceClaim, MachinePlanError,
    MachinePlanResult, VmMachinePlan,
};

/// One claimed host device whose destructor restores the complete host state.
///
/// Implementations own any saved IRQ route, priority, trigger, DMA, power, and
/// driver state needed to return the device to the host. Dropping the lease is
/// the only rollback operation used by the generic transaction.
pub trait HostDeviceLease: Send {}

/// One retained provider-local resource whose destructor releases its claim.
///
/// Implementations keep a clock, reset, or other provider-owned resource in
/// the state recorded by the immutable machine plan. The device lease is
/// released before this supporting-resource lease during rollback and VM drop.
pub trait HostProviderResourceLease: Send {}

/// Platform capability used to atomically claim the physical devices and
/// provider-local dependencies in a VM machine plan.
pub trait HostDeviceClaimProvider {
    /// Returns the current generation of the live platform inventory.
    fn snapshot_generation(&self) -> u64;

    /// Claims one device and returns a lease that restores it when dropped.
    fn claim(&self, device: &HostDeviceId) -> MachinePlanResult<Box<dyn HostDeviceLease>>;

    /// Retains one provider-local resource for the complete device lease.
    fn claim_provider_resource(
        &self,
        resource: &HostProviderResourceClaim,
    ) -> MachinePlanResult<Box<dyn HostProviderResourceLease>>;
}

static HOST_DEVICE_OWNERS: Mutex<BTreeMap<HostDeviceId, usize>> = Mutex::new(BTreeMap::new());
static HOST_PROVIDER_RESOURCE_OWNERS: Mutex<BTreeMap<HostProviderResourceKey, usize>> =
    Mutex::new(BTreeMap::new());

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct HostProviderResourceKey {
    provider: HostDeviceId,
    reference: HostProviderReference,
}

impl HostProviderResourceKey {
    fn from_claim(claim: &HostProviderResourceClaim) -> Self {
        Self {
            provider: claim.provider().clone(),
            reference: claim.grant().reference().clone(),
        }
    }
}

/// VM-level claim provider that serializes planned host-resource ownership.
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

    fn claim_provider_resource(
        &self,
        resource: &HostProviderResourceClaim,
    ) -> MachinePlanResult<Box<dyn HostProviderResourceLease>> {
        let key = HostProviderResourceKey::from_claim(resource);
        let mut owners = HOST_PROVIDER_RESOURCE_OWNERS.lock();
        if let Some(owner) = owners.get(&key) {
            return Err(MachinePlanError::ClaimRejected {
                device: resource.provider().to_string(),
                detail: format!(
                    "provider resource {:?} selector {:?} is already owned by VM {owner}",
                    resource.grant().reference().kind(),
                    resource.grant().reference().specifier(),
                ),
            });
        }
        owners.insert(key.clone(), self.vm_id);
        Ok(Box::new(RegisteredHostProviderResourceLease {
            key,
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

struct RegisteredHostProviderResourceLease {
    key: HostProviderResourceKey,
    vm_id: usize,
}

impl HostProviderResourceLease for RegisteredHostProviderResourceLease {}

impl Drop for RegisteredHostProviderResourceLease {
    fn drop(&mut self) {
        let mut owners = HOST_PROVIDER_RESOURCE_OWNERS.lock();
        if owners.get(&self.key) == Some(&self.vm_id) {
            owners.remove(&self.key);
        }
    }
}

struct PendingHostLeases {
    devices: Vec<Box<dyn HostDeviceLease>>,
    provider_resources: Vec<Box<dyn HostProviderResourceLease>>,
}

impl PendingHostLeases {
    fn new(device_capacity: usize, provider_resource_capacity: usize) -> Self {
        Self {
            devices: Vec::with_capacity(device_capacity),
            provider_resources: Vec::with_capacity(provider_resource_capacity),
        }
    }

    fn len(&self) -> usize {
        self.devices.len() + self.provider_resources.len()
    }

    fn is_empty(&self) -> bool {
        self.devices.is_empty() && self.provider_resources.is_empty()
    }

    fn release_all(&mut self) {
        while self.devices.pop().is_some() {}
        while self.provider_resources.pop().is_some() {}
    }
}

impl Drop for PendingHostLeases {
    fn drop(&mut self) {
        self.release_all();
    }
}

/// In-progress host-resource ownership transaction.
///
/// If construction or any later VM build stage fails, dropping this value
/// releases devices before their supporting provider resources, with each
/// category released in reverse acquisition order.
pub struct VmMachineTransaction {
    leases: Option<PendingHostLeases>,
}

impl VmMachineTransaction {
    /// Revalidates the snapshot generation and claims every planned resource.
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

        let mut leases =
            PendingHostLeases::new(plan.claims().len(), plan.provider_resource_claims().len());
        for resource in plan.provider_resource_claims() {
            leases
                .provider_resources
                .push(provider.claim_provider_resource(resource)?);
        }
        for device in plan.claims() {
            leases.devices.push(provider.claim(device)?);
        }
        Ok(Self {
            leases: Some(leases),
        })
    }

    /// Converts the transaction into VM-owned leases after all build stages
    /// have succeeded.
    pub fn commit(mut self) -> HostDeviceLeases {
        let leases = self
            .leases
            .take()
            .expect("a transaction can commit its lease collection exactly once");
        HostDeviceLeases { leases }
    }

    /// Returns the number of device and provider-resource leases held.
    pub fn len(&self) -> usize {
        self.leases.as_ref().map_or(0, PendingHostLeases::len)
    }

    /// Returns whether this transaction owns no host resources.
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

/// Host-resource leases retained for the complete lifetime of a committed VM.
pub struct HostDeviceLeases {
    leases: PendingHostLeases,
}

impl HostDeviceLeases {
    /// Returns how many device and provider-resource leases are owned by the VM.
    pub fn len(&self) -> usize {
        self.leases.len()
    }

    /// Returns whether the VM owns no physical resources.
    pub fn is_empty(&self) -> bool {
        self.leases.is_empty()
    }
}

impl core::fmt::Debug for HostDeviceLeases {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("HostDeviceLeases")
            .field("device_lease_count", &self.leases.devices.len())
            .field(
                "provider_resource_lease_count",
                &self.leases.provider_resources.len(),
            )
            .finish()
    }
}
