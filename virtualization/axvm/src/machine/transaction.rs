//! Transactional ownership transfer for planned host devices.

use alloc::{
    boxed::Box,
    collections::BTreeMap,
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

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
pub trait HostProviderResourceLease: Send + Sync {
    /// Returns which runtime operation domain this lease exposes.
    fn control_kind(&self) -> HostProviderResourceControlKind {
        HostProviderResourceControlKind::Pinned
    }

    /// Borrows the clock capability owned by this lease, when present.
    fn clock_control(&self) -> Option<&dyn HostClockControl> {
        None
    }

    /// Borrows the reset capability owned by this lease, when present.
    fn reset_control(&self) -> Option<&dyn HostResetControl> {
        None
    }
}

/// Runtime failure returned by a mediated host-provider capability.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HostProviderControlError {
    /// The selected resource or operation is not implemented by the backend.
    #[error("host provider operation '{operation}' is unsupported")]
    Unsupported {
        /// Operation rejected by the backend.
        operation: &'static str,
    },
    /// The live provider failed an otherwise valid operation.
    #[error("host provider operation '{operation}' failed: {detail}")]
    Backend {
        /// Operation attempted by the mediator.
        operation: &'static str,
        /// Platform-specific diagnostic detail.
        detail: String,
    },
}

/// Lease-bound operations for one assigned host clock.
pub trait HostClockControl: Send + Sync {
    /// Returns whether the physical clock is currently enabled.
    fn is_enabled(&self) -> Result<bool, HostProviderControlError>;

    /// Ensures that the physical clock is enabled.
    fn enable(&self) -> Result<(), HostProviderControlError>;

    /// Returns the current physical rate in hertz.
    fn rate(&self) -> Result<u64, HostProviderControlError>;

    /// Requests a new physical rate in hertz.
    fn set_rate(&self, rate_hz: u64) -> Result<(), HostProviderControlError>;
}

/// Lease-bound operations for one assigned host reset line.
pub trait HostResetControl: Send + Sync {
    /// Returns whether the line is currently asserted.
    fn is_asserted(&self) -> Result<bool, HostProviderControlError>;

    /// Asserts the physical reset line.
    fn assert(&self) -> Result<(), HostProviderControlError>;

    /// Deasserts the physical reset line.
    fn deassert(&self) -> Result<(), HostProviderControlError>;
}

/// Operation domain exposed by one retained provider-resource lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostProviderResourceControlKind {
    /// The resource is statically pinned and has no runtime operations.
    Pinned,
    /// The lease exposes clock operations.
    Clock,
    /// The lease exposes reset operations.
    Reset,
}

/// A clock capability that structurally retains its ownership lease.
#[derive(Clone)]
pub(crate) struct LeasedHostClock {
    lease: Arc<dyn HostProviderResourceLease>,
}

impl LeasedHostClock {
    fn new(lease: Arc<dyn HostProviderResourceLease>) -> Self {
        Self { lease }
    }

    fn control(&self) -> Result<&dyn HostClockControl, HostProviderControlError> {
        self.lease
            .clock_control()
            .ok_or(HostProviderControlError::Unsupported {
                operation: "access leased host clock",
            })
    }

    /// Returns whether the assigned clock is physically enabled.
    pub(crate) fn is_enabled(&self) -> Result<bool, HostProviderControlError> {
        self.control()?.is_enabled()
    }

    /// Ensures that the assigned clock is physically enabled.
    pub(crate) fn enable(&self) -> Result<(), HostProviderControlError> {
        self.control()?.enable()
    }

    /// Returns the assigned clock's current physical rate.
    pub(crate) fn rate(&self) -> Result<u64, HostProviderControlError> {
        self.control()?.rate()
    }

    /// Requests a new physical rate for the assigned clock.
    pub(crate) fn set_rate(&self, rate_hz: u64) -> Result<(), HostProviderControlError> {
        self.control()?.set_rate(rate_hz)
    }
}

impl core::fmt::Debug for LeasedHostClock {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("LeasedHostClock(..)")
    }
}

/// A reset capability that structurally retains its ownership lease.
#[derive(Clone)]
pub(crate) struct LeasedHostReset {
    lease: Arc<dyn HostProviderResourceLease>,
}

impl LeasedHostReset {
    fn new(lease: Arc<dyn HostProviderResourceLease>) -> Self {
        Self { lease }
    }

    fn control(&self) -> Result<&dyn HostResetControl, HostProviderControlError> {
        self.lease
            .reset_control()
            .ok_or(HostProviderControlError::Unsupported {
                operation: "access leased host reset",
            })
    }

    /// Returns whether the assigned reset line is asserted.
    pub(crate) fn is_asserted(&self) -> Result<bool, HostProviderControlError> {
        self.control()?.is_asserted()
    }

    /// Asserts the assigned reset line.
    pub(crate) fn assert(&self) -> Result<(), HostProviderControlError> {
        self.control()?.assert()
    }

    /// Deasserts the assigned reset line.
    pub(crate) fn deassert(&self) -> Result<(), HostProviderControlError> {
        self.control()?.deassert()
    }
}

impl core::fmt::Debug for LeasedHostReset {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("LeasedHostReset(..)")
    }
}

/// Runtime authority carried by one provider-resource lease.
#[derive(Clone, Debug)]
pub(crate) enum HostProviderResourceControl {
    /// The lease pins static state and exposes no guest operation.
    Pinned,
    /// Mutable access to one assigned physical clock.
    Clock(LeasedHostClock),
    /// Mutable access to one assigned physical reset line.
    Reset(LeasedHostReset),
}

impl HostProviderResourceControl {
    fn from_lease(lease: Arc<dyn HostProviderResourceLease>) -> Self {
        match lease.control_kind() {
            HostProviderResourceControlKind::Pinned => Self::Pinned,
            HostProviderResourceControlKind::Clock => Self::Clock(LeasedHostClock::new(lease)),
            HostProviderResourceControlKind::Reset => Self::Reset(LeasedHostReset::new(lease)),
        }
    }
}

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
    ) -> MachinePlanResult<Arc<dyn HostProviderResourceLease>>;
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
    ) -> MachinePlanResult<Arc<dyn HostProviderResourceLease>> {
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
        Ok(Arc::new(RegisteredHostProviderResourceLease {
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
    provider_resources: Vec<ClaimedProviderResource>,
}

struct ClaimedProviderResource {
    claim: HostProviderResourceClaim,
    lease: Arc<dyn HostProviderResourceLease>,
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
        while let Some(resource) = self.provider_resources.pop() {
            drop(resource.lease);
        }
    }

    fn provider_control(
        &self,
        claim: &HostProviderResourceClaim,
    ) -> Option<HostProviderResourceControl> {
        self.provider_resources
            .iter()
            .find(|resource| &resource.claim == claim)
            .map(|resource| HostProviderResourceControl::from_lease(resource.lease.clone()))
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
            let lease = provider.claim_provider_resource(resource)?;
            validate_provider_resource_lease(resource, lease.as_ref())?;
            leases.provider_resources.push(ClaimedProviderResource {
                claim: resource.clone(),
                lease,
            });
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

    /// Returns the lease-bound runtime capability for one planned resource.
    pub(crate) fn provider_control(
        &self,
        claim: &HostProviderResourceClaim,
    ) -> Option<HostProviderResourceControl> {
        self.leases.as_ref()?.provider_control(claim)
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

    /// Returns the lease-bound runtime capability for one planned resource.
    pub(crate) fn provider_control(
        &self,
        claim: &HostProviderResourceClaim,
    ) -> Option<HostProviderResourceControl> {
        self.leases.provider_control(claim)
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

fn validate_provider_resource_lease(
    resource: &HostProviderResourceClaim,
    lease: &dyn HostProviderResourceLease,
) -> MachinePlanResult<()> {
    let expected = match resource.grant().state() {
        super::HostProviderResourceState::FixedClock(_)
        | super::HostProviderResourceState::DeassertedReset => {
            HostProviderResourceControlKind::Pinned
        }
        super::HostProviderResourceState::MediatedClock => HostProviderResourceControlKind::Clock,
        super::HostProviderResourceState::MediatedReset => HostProviderResourceControlKind::Reset,
    };
    let actual = lease.control_kind();
    let capability_present = match actual {
        HostProviderResourceControlKind::Pinned => true,
        HostProviderResourceControlKind::Clock => lease.clock_control().is_some(),
        HostProviderResourceControlKind::Reset => lease.reset_control().is_some(),
    };
    if actual == expected && capability_present {
        return Ok(());
    }
    Err(MachinePlanError::ClaimRejected {
        device: resource.provider().to_string(),
        detail: format!(
            "provider selector {:?} requires {expected:?} control, but the lease exposes \
             {actual:?} with capability_present={capability_present}",
            resource.grant().reference().specifier(),
        ),
    })
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn runtime_control_cannot_outlive_its_ownership_lease() {
        let drops = Arc::new(AtomicUsize::new(0));
        let rate = Arc::new(AtomicU64::new(24_000_000));
        let lease: Arc<dyn HostProviderResourceLease> = Arc::new(ClockLease {
            drops: drops.clone(),
            clock: TestClock { rate: rate.clone() },
        });
        let control = HostProviderResourceControl::from_lease(lease);
        let HostProviderResourceControl::Clock(clock) = control else {
            panic!("clock lease produced a non-clock control");
        };

        assert!(clock.is_enabled().unwrap());
        clock.enable().unwrap();
        clock.set_rate(375_000).unwrap();
        assert_eq!(clock.rate().unwrap(), 375_000);
        assert_eq!(rate.load(Ordering::Acquire), 375_000);
        assert_eq!(drops.load(Ordering::Acquire), 0);

        drop(clock);
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }

    #[test]
    fn reset_control_retains_and_uses_only_its_lease() {
        let drops = Arc::new(AtomicUsize::new(0));
        let asserted = Arc::new(AtomicBool::new(false));
        let lease: Arc<dyn HostProviderResourceLease> = Arc::new(ResetLease {
            drops: drops.clone(),
            reset: TestReset {
                asserted: asserted.clone(),
            },
        });
        let control = HostProviderResourceControl::from_lease(lease);
        let HostProviderResourceControl::Reset(reset) = control else {
            panic!("reset lease produced a non-reset control");
        };

        assert!(!reset.is_asserted().unwrap());
        reset.assert().unwrap();
        assert!(asserted.load(Ordering::Acquire));
        reset.deassert().unwrap();
        assert!(!asserted.load(Ordering::Acquire));
        assert_eq!(drops.load(Ordering::Acquire), 0);

        drop(reset);
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }

    struct ClockLease {
        drops: Arc<AtomicUsize>,
        clock: TestClock,
    }

    impl HostProviderResourceLease for ClockLease {
        fn control_kind(&self) -> HostProviderResourceControlKind {
            HostProviderResourceControlKind::Clock
        }

        fn clock_control(&self) -> Option<&dyn HostClockControl> {
            Some(&self.clock)
        }
    }

    impl Drop for ClockLease {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::AcqRel);
        }
    }

    struct TestClock {
        rate: Arc<AtomicU64>,
    }

    impl HostClockControl for TestClock {
        fn is_enabled(&self) -> Result<bool, HostProviderControlError> {
            Ok(true)
        }

        fn enable(&self) -> Result<(), HostProviderControlError> {
            Ok(())
        }

        fn rate(&self) -> Result<u64, HostProviderControlError> {
            Ok(self.rate.load(Ordering::Acquire))
        }

        fn set_rate(&self, rate_hz: u64) -> Result<(), HostProviderControlError> {
            self.rate.store(rate_hz, Ordering::Release);
            Ok(())
        }
    }

    struct ResetLease {
        drops: Arc<AtomicUsize>,
        reset: TestReset,
    }

    impl HostProviderResourceLease for ResetLease {
        fn control_kind(&self) -> HostProviderResourceControlKind {
            HostProviderResourceControlKind::Reset
        }

        fn reset_control(&self) -> Option<&dyn HostResetControl> {
            Some(&self.reset)
        }
    }

    impl Drop for ResetLease {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::AcqRel);
        }
    }

    struct TestReset {
        asserted: Arc<AtomicBool>,
    }

    impl HostResetControl for TestReset {
        fn is_asserted(&self) -> Result<bool, HostProviderControlError> {
            Ok(self.asserted.load(Ordering::Acquire))
        }

        fn assert(&self) -> Result<(), HostProviderControlError> {
            self.asserted.store(true, Ordering::Release);
            Ok(())
        }

        fn deassert(&self) -> Result<(), HostProviderControlError> {
            self.asserted.store(false, Ordering::Release);
            Ok(())
        }
    }
}
