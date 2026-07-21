//! Registry boundary for move-only rdif-block v0.13 controller activators.

use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};
use core::fmt;

use ax_errno::AxError;
use log::warn;
use rdif_block::{
    BControllerActivator, ControlDomainCapability, ControllerActivator, ControllerCapabilities,
    DriverGeneric, IdList,
};
use rdrive::{Device, DeviceId, probe::OnProbeError};

use super::{
    binding::{BlockDeviceBinding, BlockRegistrationError},
    deferred_irq::{
        PendingBlockIrqRealizationParts, PlatformIrqActivator, PlatformIrqBindingState,
    },
};
use crate::{
    BindingInfo, BindingIrq, BindingLocator, HostMmioRange, IrqBindingLease,
    binding_info_from_acpi, binding_info_from_fdt,
    registration::{BoundDevice, register_bound_device},
};

/// Driver-core activator retained by `rdrive` until a runtime selects a plan.
struct PlatformBlockActivator {
    name: String,
    capabilities: ControllerCapabilities,
    activator: Option<BControllerActivator>,
    platform_irq: Option<PlatformIrqBindingState>,
    binding: BlockDeviceBinding,
}

struct TakenPlatformBlockActivator {
    activator: BControllerActivator,
    capabilities: ControllerCapabilities,
    platform_irq: PlatformIrqBindingState,
}

impl PlatformBlockActivator {
    fn new(
        device_id: DeviceId,
        name: String,
        activator: BControllerActivator,
        binding: BindingInfo,
        irq_lease: Option<Box<dyn IrqBindingLease>>,
    ) -> Self {
        let capabilities = activator.capabilities().clone();
        Self {
            name,
            capabilities,
            activator: Some(activator),
            platform_irq: Some(PlatformIrqBindingState::realized(irq_lease)),
            binding: BlockDeviceBinding::new(device_id, binding),
        }
    }

    fn new_deferred(
        device_id: DeviceId,
        name: String,
        activator: BControllerActivator,
        platform_irq: Box<dyn PlatformIrqActivator>,
    ) -> Self {
        let capabilities = activator.capabilities().clone();
        let binding = platform_irq.discovery_binding().clone();
        Self {
            name,
            capabilities,
            activator: Some(activator),
            platform_irq: Some(PlatformIrqBindingState::deferred(platform_irq)),
            binding: BlockDeviceBinding::new(device_id, binding),
        }
    }

    fn take_activator(&mut self) -> Option<TakenPlatformBlockActivator> {
        let activator = self.activator.as_ref()?;
        if activator.capabilities() != &self.capabilities {
            return None;
        }
        Some(TakenPlatformBlockActivator {
            activator: self.activator.take()?,
            capabilities: self.capabilities.clone(),
            platform_irq: self.platform_irq.take()?,
        })
    }
}

impl DriverGeneric for PlatformBlockActivator {
    fn name(&self) -> &str {
        &self.name
    }
}

impl BoundDevice for PlatformBlockActivator {
    fn binding_info(&self) -> &BindingInfo {
        self.binding.platform_binding()
    }
}

/// A discovered controller waiting for one immutable runtime activation plan.
///
/// The contained activator is move-only. Query capabilities before calling
/// [`Self::into_parts`], then move the activator and optional IRQ lease into
/// exactly one runtime activation owner.
pub struct RdifBlockActivator {
    name: String,
    binding: BlockDeviceBinding,
    capabilities: ControllerCapabilities,
    activator: BControllerActivator,
    platform_irq: PlatformIrqBindingState,
}

impl fmt::Debug for RdifBlockActivator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RdifBlockActivator")
            .field("name", &self.name)
            .field("binding", &self.binding)
            .field("capabilities", &self.capabilities)
            .field("platform_irq", &self.platform_irq)
            .finish_non_exhaustive()
    }
}

impl RdifBlockActivator {
    /// Returns the portable driver-reported controller name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the immutable controller capabilities used to build a plan.
    pub fn capabilities(&self) -> &ControllerCapabilities {
        &self.capabilities
    }

    /// Returns the complete host-resource binding captured during discovery.
    pub const fn binding(&self) -> &BlockDeviceBinding {
        &self.binding
    }

    /// Returns the stable `rdrive` registry identity.
    pub const fn device_id(&self) -> DeviceId {
        self.binding.device_id()
    }

    /// Returns the firmware or bus locator captured during discovery.
    pub const fn locator(&self) -> &BindingLocator {
        self.binding.locator()
    }

    /// Returns every validated host MMIO or PCI memory-BAR range.
    pub fn host_mmio_ranges(&self) -> &[HostMmioRange] {
        self.binding.host_mmio_ranges()
    }

    /// Returns the platform IRQ bound to a portable source identity.
    pub fn irq_for_source(&self, source_id: usize) -> Option<&BindingIrq> {
        self.binding.irq_for_source(source_id)
    }

    /// Returns every unresolved source-to-platform IRQ binding.
    pub fn irq_sources(&self) -> &[crate::BindingIrqBinding] {
        self.binding.irq_sources()
    }

    /// Whether activation carries a move-only platform IRQ lease.
    pub const fn has_irq_binding_lease(&self) -> bool {
        self.platform_irq.has_realized_lease()
    }

    /// Whether platform IRQ allocation is waiting for the runtime plan.
    pub const fn has_deferred_irq_binding(&self) -> bool {
        self.platform_irq.is_deferred()
    }

    pub(super) fn into_irq_realization_parts(self) -> PendingBlockIrqRealizationParts {
        PendingBlockIrqRealizationParts {
            name: self.name,
            binding: self.binding,
            capabilities: self.capabilities,
            activator: self.activator,
            platform_irq: self.platform_irq,
        }
    }

    pub(super) fn from_irq_realization_parts(parts: PendingBlockIrqRealizationParts) -> Self {
        Self {
            name: parts.name,
            binding: parts.binding,
            capabilities: parts.capabilities,
            activator: parts.activator,
            platform_irq: parts.platform_irq,
        }
    }
}

impl TryFrom<Device<PlatformBlockActivator>> for RdifBlockActivator {
    type Error = AxError;

    fn try_from(base: Device<PlatformBlockActivator>) -> Result<Self, Self::Error> {
        let mut device = base.lock().map_err(|_| AxError::BadState)?;
        let name = device.name.clone();
        let binding = device.binding.clone();
        let taken = device.take_activator().ok_or(AxError::BadState)?;
        Ok(Self {
            name,
            binding,
            capabilities: taken.capabilities,
            activator: taken.activator,
            platform_irq: taken.platform_irq,
        })
    }
}

/// Registers controllers through the rdif-block v0.13 activation boundary.
pub trait PlatformDeviceBlockActivation {
    /// Registers an activator without firmware resource metadata.
    fn register_block_activator<T: ControllerActivator>(self, activator: T) -> Option<usize>;

    /// Registers an activator with validated platform resource metadata.
    ///
    /// PCI facts are rejected here because they do not own an INTx or MSI-X
    /// allocation. PCI callers must use
    /// [`Self::register_irq_bound_block_activator`] after taking that lease.
    fn register_block_activator_with_info<T: ControllerActivator>(
        self,
        activator: T,
        binding: BindingInfo,
    ) -> Option<usize>;

    /// Registers an activator together with its move-only platform IRQ lease.
    ///
    /// The lease is transferred through `rdrive`, survives plan validation,
    /// and leaves only through [`super::RdifBlockRealizedActivator::into_parts`];
    /// it is never reconstructed from [`BindingInfo`].
    fn register_irq_bound_block_activator<T, L>(self, activator: T, irq_lease: L) -> Option<usize>
    where
        T: ControllerActivator,
        L: IrqBindingLease;

    /// Registers a controller whose exact platform IRQ allocation is selected
    /// only after the runtime freezes its immutable activation plan.
    fn register_deferred_irq_block_activator<T, P>(
        self,
        activator: T,
        platform_irq: P,
    ) -> Option<usize>
    where
        T: ControllerActivator,
        P: PlatformIrqActivator;
}

impl PlatformDeviceBlockActivation for rdrive::PlatformDevice {
    fn register_block_activator<T: ControllerActivator>(self, activator: T) -> Option<usize> {
        self.register_block_activator_with_info(activator, BindingInfo::empty())
    }

    fn register_block_activator_with_info<T: ControllerActivator>(
        self,
        activator: T,
        binding: BindingInfo,
    ) -> Option<usize> {
        register_block_activator_with_info(self, activator, binding)
    }

    fn register_irq_bound_block_activator<T, L>(self, activator: T, irq_lease: L) -> Option<usize>
    where
        T: ControllerActivator,
        L: IrqBindingLease,
    {
        let binding = irq_lease.binding_info();
        register_block_activator_with_lease(self, activator, binding, Box::new(irq_lease))
    }

    fn register_deferred_irq_block_activator<T, P>(
        self,
        activator: T,
        platform_irq: P,
    ) -> Option<usize>
    where
        T: ControllerActivator,
        P: PlatformIrqActivator,
    {
        register_deferred_block_activator(self, activator, Box::new(platform_irq))
    }
}

/// Registers an FDT-discovered controller activator.
pub trait ProbeFdtBlockActivation {
    fn register_block_activator<T: ControllerActivator>(
        self,
        activator: T,
    ) -> Result<Option<usize>, OnProbeError>;
}

impl ProbeFdtBlockActivation for rdrive::probe::fdt::ProbeFdt<'_> {
    fn register_block_activator<T: ControllerActivator>(
        self,
        activator: T,
    ) -> Result<Option<usize>, OnProbeError> {
        let binding = binding_info_from_fdt(self.info())?;
        validate_activator_irq_bindings(&activator, &binding)
            .map_err(block_activation_probe_error)?;
        Ok(register_block_activator_with_info(
            self.into_platform_device(),
            activator,
            binding,
        ))
    }
}

/// Registers an ACPI-discovered controller activator.
pub trait ProbeAcpiBlockActivation {
    fn register_block_activator<T: ControllerActivator>(
        self,
        activator: T,
    ) -> Result<Option<usize>, OnProbeError>;
}

impl ProbeAcpiBlockActivation for rdrive::probe::acpi::ProbeAcpi<'_> {
    fn register_block_activator<T: ControllerActivator>(
        self,
        activator: T,
    ) -> Result<Option<usize>, OnProbeError> {
        let binding = binding_info_from_acpi(self.info())?;
        validate_activator_irq_bindings(&activator, &binding)
            .map_err(block_activation_probe_error)?;
        Ok(register_block_activator_with_info(
            self.into_platform_device(),
            activator,
            binding,
        ))
    }
}

/// Transfers every v0.13 controller activator to the OS block runtime.
///
/// Legacy [`super::RdifBlockDevice`] registrations remain in their distinct
/// registry type and cannot be consumed through this function.
pub fn take_rdif_block_activators() -> Vec<RdifBlockActivator> {
    rdrive::get_list::<PlatformBlockActivator>()
        .into_iter()
        .filter_map(|device| match RdifBlockActivator::try_from(device) {
            Ok(activator) => Some(activator),
            Err(error) => {
                warn!("failed to take RDIF block activator: {error:?}");
                None
            }
        })
        .collect()
}

fn register_block_activator_with_info<T: ControllerActivator>(
    platform: rdrive::PlatformDevice,
    activator: T,
    binding: BindingInfo,
) -> Option<usize> {
    if let Err(error) = validate_fact_only_activator_registration(&activator, &binding) {
        warn!(
            "refusing to register block controller activator {}: {error}",
            activator.name()
        );
        return None;
    }
    register_validated_block_activator(platform, activator, binding, None)
}

fn register_block_activator_with_lease<T: ControllerActivator>(
    platform: rdrive::PlatformDevice,
    activator: T,
    binding: BindingInfo,
    irq_lease: Box<dyn IrqBindingLease>,
) -> Option<usize> {
    if let Err(error) = validate_activator_irq_bindings(&activator, &binding) {
        warn!(
            "refusing to register IRQ-bound block controller activator {}: {error}",
            activator.name()
        );
        return None;
    }
    register_validated_block_activator(platform, activator, binding, Some(irq_lease))
}

fn register_deferred_block_activator<T: ControllerActivator>(
    platform: rdrive::PlatformDevice,
    activator: T,
    platform_irq: Box<dyn PlatformIrqActivator>,
) -> Option<usize> {
    if let Err(error) = validate_deferred_activator_registration(&activator, &*platform_irq) {
        warn!(
            "refusing to register deferred-IRQ block controller activator {}: {error}",
            activator.name()
        );
        return None;
    }
    let name = activator.name().into();
    let device_id = platform.descriptor().device_id();
    register_bound_device(
        platform,
        PlatformBlockActivator::new_deferred(device_id, name, Box::new(activator), platform_irq),
    )
}

fn register_validated_block_activator<T: ControllerActivator>(
    platform: rdrive::PlatformDevice,
    activator: T,
    binding: BindingInfo,
    irq_lease: Option<Box<dyn IrqBindingLease>>,
) -> Option<usize> {
    let name = activator.name().into();
    let device_id = platform.descriptor().device_id();
    register_bound_device(
        platform,
        PlatformBlockActivator::new(device_id, name, Box::new(activator), binding, irq_lease),
    )
}

fn validate_deferred_activator_registration(
    _activator: &dyn ControllerActivator,
    platform_irq: &dyn PlatformIrqActivator,
) -> Result<(), BlockRegistrationError> {
    if !platform_irq.discovery_binding().irq_sources().is_empty() {
        return Err(BlockRegistrationError::DeferredBindingContainsIrqSource);
    }
    Ok(())
}

fn validate_fact_only_activator_registration(
    activator: &dyn ControllerActivator,
    binding: &BindingInfo,
) -> Result<(), BlockRegistrationError> {
    if matches!(binding.locator(), BindingLocator::Pci { .. }) {
        return Err(BlockRegistrationError::PciIrqLeaseRequired);
    }
    validate_activator_irq_bindings(activator, binding)
}

fn validate_activator_irq_bindings(
    activator: &dyn ControllerActivator,
    binding: &BindingInfo,
) -> Result<(), BlockRegistrationError> {
    let capabilities = activator.capabilities();
    if let ControlDomainCapability::Independent { irq_sources, .. } =
        capabilities.control_capability()
    {
        validate_irq_source_bindings(irq_sources, binding)?;
    }
    for domain in capabilities.domains() {
        validate_irq_source_bindings(domain.irq_sources(), binding)?;
    }
    Ok(())
}

fn validate_irq_source_bindings(
    sources: IdList,
    binding: &BindingInfo,
) -> Result<(), BlockRegistrationError> {
    for source_id in sources.iter() {
        if binding.irq_for_source(source_id).is_none() {
            return Err(BlockRegistrationError::MissingIrqBinding { source_id });
        }
    }
    Ok(())
}

fn block_activation_probe_error(error: BlockRegistrationError) -> OnProbeError {
    OnProbeError::other(error.to_string())
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, string::String, sync::Arc, vec};
    use core::{
        num::{NonZeroU16, NonZeroU64, NonZeroUsize},
        sync::atomic::{AtomicBool, AtomicU64, Ordering},
    };

    use rdif_block::{
        ActivationError, ActivationFailure, ActivationPlan, ControlDomainCapability,
        DomainActivationPlan, DriverDeviceKey, HardwareQueueDepth, IdList, LogicalDeviceCapability,
        LogicalDeviceConstraints, LogicalDeviceSelector, OwnershipDomainCapability,
        OwnershipDomainId, PreparedControllerParts, QueueExecution,
    };

    use super::*;
    use crate::block::{
        PlatformIrqActivationError, PlatformIrqActivationFailure, RdifBlockActivationParts,
        RealizedPlatformIrqBinding,
    };

    struct ValidationActivator {
        capabilities: ControllerCapabilities,
    }

    struct TestIrqLease {
        binding: BindingInfo,
        dropped: Arc<AtomicBool>,
    }

    struct PlanSelectedIrqActivator {
        discovery: BindingInfo,
        realized_sources: Arc<AtomicU64>,
        lease_dropped: Arc<AtomicBool>,
        fail_returned: bool,
    }

    impl IrqBindingLease for TestIrqLease {
        fn binding_info(&self) -> BindingInfo {
            self.binding.clone()
        }

        fn enable_binding_irq(&self) -> Result<(), crate::IrqBindingError> {
            Ok(())
        }

        fn disable_binding_irq(&self) -> Result<(), crate::IrqBindingError> {
            Ok(())
        }
    }

    impl Drop for TestIrqLease {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

    impl PlatformIrqActivator for PlanSelectedIrqActivator {
        fn discovery_binding(&self) -> &BindingInfo {
            &self.discovery
        }

        fn realize(
            self: Box<Self>,
            plan: &ActivationPlan,
        ) -> Result<RealizedPlatformIrqBinding, PlatformIrqActivationFailure> {
            let mut source_bits = plan.control_activation().irq_sources().bits();
            for domain in plan.domains() {
                source_bits |= domain.irq_sources().bits();
            }
            self.realized_sources.store(source_bits, Ordering::Release);
            if self.fail_returned {
                return Err(PlatformIrqActivationFailure::returned(
                    PlatformIrqActivationError::Returned,
                    *self,
                ));
            }
            let binding = BindingInfo::with_irq_sources(
                rdif_block::IdList::from_bits(source_bits)
                    .iter()
                    .map(|source_id| {
                        (
                            source_id,
                            BindingIrq::acpi_gsi(u32::try_from(32 + source_id).unwrap()),
                        )
                    }),
            )
            .with_host_resources(
                self.discovery.locator().clone(),
                self.discovery.host_mmio_ranges().to_vec(),
            );
            Ok(RealizedPlatformIrqBinding::new(TestIrqLease {
                binding,
                dropped: Arc::clone(&self.lease_dropped),
            }))
        }
    }

    impl DriverGeneric for ValidationActivator {
        fn name(&self) -> &str {
            "validation-activator"
        }
    }

    impl ControllerActivator for ValidationActivator {
        fn capabilities(&self) -> &ControllerCapabilities {
            &self.capabilities
        }

        fn activate(
            self: Box<Self>,
            _plan: ActivationPlan,
        ) -> Result<PreparedControllerParts, ActivationFailure> {
            Err(ActivationFailure::new(
                ActivationError::ControllerIdentityMismatch,
                self,
            ))
        }
    }

    #[test]
    fn activation_registration_requires_every_capability_irq_binding() {
        let activator = validation_activator(IdList::from_bits((1 << 3) | (1 << 7)));
        let normal_only = BindingInfo::with_irq_sources([(3, BindingIrq::acpi_gsi(19))]);

        assert_eq!(
            validate_activator_irq_bindings(&activator, &normal_only),
            Err(BlockRegistrationError::MissingIrqBinding { source_id: 7 })
        );

        let all_sources = BindingInfo::with_irq_sources([
            (3, BindingIrq::acpi_gsi(19)),
            (7, BindingIrq::acpi_gsi(20)),
        ]);
        assert_eq!(
            validate_activator_irq_bindings(&activator, &all_sources),
            Ok(())
        );
    }

    #[test]
    fn independent_control_irq_must_have_its_own_platform_binding() {
        let activator = validation_activator_with_independent_control(
            IdList::from_bits(1 << 3),
            IdList::from_bits(1 << 7),
        );
        let io_only = BindingInfo::with_irq_sources([(3, BindingIrq::acpi_gsi(19))]);

        assert_eq!(
            validate_activator_irq_bindings(&activator, &io_only),
            Err(BlockRegistrationError::MissingIrqBinding { source_id: 7 })
        );
    }

    #[test]
    fn registered_activator_is_move_only_and_can_be_taken_once() {
        let binding = BindingInfo::with_irq_sources([(3, BindingIrq::acpi_gsi(19))]);
        let mut registered = PlatformBlockActivator::new(
            DeviceId::from(73),
            String::from("validation-activator"),
            Box::new(validation_activator(IdList::from_bits(1 << 3))),
            binding,
            None,
        );

        let taken = registered
            .take_activator()
            .expect("the unique activator must be available once");
        assert_eq!(
            taken.capabilities.controller_identity(),
            NonZeroUsize::new(0x51).unwrap()
        );
        assert_eq!(taken.activator.capabilities(), &taken.capabilities);
        assert!(matches!(
            taken.platform_irq,
            PlatformIrqBindingState::Realized { irq_lease: None }
        ));
        assert!(registered.take_activator().is_none());
    }

    #[test]
    fn irq_bound_registration_transfers_the_live_lease_with_the_activator() {
        let dropped = Arc::new(AtomicBool::new(false));
        let binding = BindingInfo::with_irq_sources([(3, BindingIrq::acpi_gsi(19))]);
        let lease = TestIrqLease {
            binding: binding.clone(),
            dropped: Arc::clone(&dropped),
        };
        let mut registered = PlatformBlockActivator::new(
            DeviceId::from(74),
            String::from("validation-activator"),
            Box::new(validation_activator(IdList::from_bits(1 << 3))),
            binding,
            Some(Box::new(lease)),
        );

        let taken = registered
            .take_activator()
            .expect("activator and lease must transfer together");
        assert!(!dropped.load(Ordering::Acquire));
        let PlatformIrqBindingState::Realized { irq_lease } = taken.platform_irq else {
            panic!("test registration must carry a realized IRQ lease");
        };
        drop(irq_lease);
        assert!(dropped.load(Ordering::Acquire));
    }

    #[test]
    fn failed_activation_retains_the_platform_irq_lease() {
        let dropped = Arc::new(AtomicBool::new(false));
        let binding = BindingInfo::with_irq_sources([(3, BindingIrq::acpi_gsi(19))]);
        let activator = validation_activator(IdList::from_bits(1 << 3));
        let capabilities = activator.capabilities().clone();
        let domain = capabilities.domains()[0].clone();
        let plan = ActivationPlan::new(
            &capabilities,
            vec![DomainActivationPlan::new(
                domain.id(),
                NonZeroU16::new(1).unwrap(),
                NonZeroU16::new(4).unwrap(),
                domain.irq_sources(),
            )],
        )
        .unwrap();
        let parts = RdifBlockActivationParts::new(
            String::from("validation-activator"),
            capabilities,
            Box::new(activator),
            Some(Box::new(TestIrqLease {
                binding: binding.clone(),
                dropped: Arc::clone(&dropped),
            })),
            BlockDeviceBinding::new(DeviceId::from(76), binding),
        );

        let failure = match parts.activate(plan) {
            Ok(_) => panic!("the validation activator must reject activation"),
            Err(failure) => failure,
        };
        assert!(!dropped.load(Ordering::Acquire));

        let (_, retained) = failure.into_parts();
        assert!(!dropped.load(Ordering::Acquire));
        assert!(retained.is_retryable());
        let retry = retained
            .into_retry_parts()
            .expect("pre-activation rejection must reconstruct the whole transaction");
        assert!(retry.has_irq_binding_lease());
        drop(retry);
        assert!(dropped.load(Ordering::Acquire));
    }

    #[test]
    fn moved_activation_binding_resolves_ready_time_io_source() {
        let source_id = 11;
        let platform_irq = BindingIrq::acpi_gsi(23);
        let binding = BlockDeviceBinding::new(
            DeviceId::from(75),
            BindingInfo::with_irq_sources([(source_id, platform_irq.clone())]),
        );
        let activator = validation_activator(IdList::from_bits(1 << source_id));
        let registered = RdifBlockActivator {
            name: String::from("validation-activator"),
            capabilities: activator.capabilities().clone(),
            activator: Box::new(activator),
            platform_irq: PlatformIrqBindingState::realized(None),
            binding,
        };
        let domain = registered.capabilities().domains()[0].clone();
        let plan = ActivationPlan::new(
            registered.capabilities(),
            vec![DomainActivationPlan::new(
                domain.id(),
                NonZeroU16::MIN,
                NonZeroU16::new(4).unwrap(),
                domain.irq_sources(),
            )],
        )
        .unwrap();
        let parts = registered
            .realize_irq_binding(&plan)
            .expect("fact-only bindings are already realized")
            .into_parts();

        assert_eq!(
            parts.binding().irq_for_source(source_id),
            Some(&platform_irq)
        );
    }

    #[test]
    fn deferred_binding_realizes_only_the_runtime_selected_optional_prefix() {
        let realized_sources = Arc::new(AtomicU64::new(0));
        let lease_dropped = Arc::new(AtomicBool::new(false));
        let discovery = pci_discovery_binding();
        let activator = optional_domain_activator();
        let capabilities = activator.capabilities().clone();
        let required = capabilities.domains()[0].clone();
        let plan = ActivationPlan::new(
            &capabilities,
            vec![DomainActivationPlan::new(
                required.id(),
                NonZeroU16::MIN,
                NonZeroU16::new(4).unwrap(),
                required.irq_sources(),
            )],
        )
        .unwrap();
        let pending = RdifBlockActivator {
            name: String::from("plan-selected"),
            binding: BlockDeviceBinding::new(DeviceId::from(88), discovery.clone()),
            capabilities,
            activator: Box::new(activator),
            platform_irq: PlatformIrqBindingState::deferred(Box::new(PlanSelectedIrqActivator {
                discovery,
                realized_sources: Arc::clone(&realized_sources),
                lease_dropped: Arc::clone(&lease_dropped),
                fail_returned: false,
            })),
        };

        assert_eq!(realized_sources.load(Ordering::Acquire), 0);
        let realized = pending.realize_irq_binding(&plan).unwrap();

        assert_eq!(realized_sources.load(Ordering::Acquire), 1);
        assert!(realized.irq_for_source(0).is_some());
        assert!(realized.irq_for_source(1).is_none());
        assert!(!lease_dropped.load(Ordering::Acquire));
        drop(realized);
        assert!(lease_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn returned_deferred_failure_reconstructs_the_complete_retry_owner() {
        let realized_sources = Arc::new(AtomicU64::new(0));
        let discovery = pci_discovery_binding();
        let activator = validation_activator(IdList::from_bits(1));
        let capabilities = activator.capabilities().clone();
        let domain = capabilities.domains()[0].clone();
        let plan = ActivationPlan::new(
            &capabilities,
            vec![DomainActivationPlan::new(
                domain.id(),
                NonZeroU16::MIN,
                NonZeroU16::new(4).unwrap(),
                domain.irq_sources(),
            )],
        )
        .unwrap();
        let pending = RdifBlockActivator {
            name: String::from("retryable"),
            binding: BlockDeviceBinding::new(DeviceId::from(89), discovery.clone()),
            capabilities,
            activator: Box::new(activator),
            platform_irq: PlatformIrqBindingState::deferred(Box::new(PlanSelectedIrqActivator {
                discovery,
                realized_sources: Arc::clone(&realized_sources),
                lease_dropped: Arc::new(AtomicBool::new(false)),
                fail_returned: true,
            })),
        };

        let failure = pending.realize_irq_binding(&plan).unwrap_err();

        assert!(failure.is_retryable());
        assert_eq!(realized_sources.load(Ordering::Acquire), 1);
        let retry = failure.into_retry_activator().unwrap();
        assert!(retry.has_deferred_irq_binding());
    }

    #[test]
    fn pci_binding_facts_without_a_move_only_irq_lease_fail_closed() {
        let activator = validation_activator(IdList::from_bits(1 << 3));
        let binding = BindingInfo::with_irq_sources([(3, BindingIrq::acpi_gsi(19))])
            .with_host_resources(
                BindingLocator::Pci {
                    segment: 0,
                    bus: 1,
                    device: 2,
                    function: 0,
                },
                vec![],
            );

        assert_eq!(
            validate_fact_only_activator_registration(&activator, &binding),
            Err(BlockRegistrationError::PciIrqLeaseRequired)
        );
    }

    fn validation_activator(irq_sources: IdList) -> ValidationActivator {
        let driver_key = DriverDeviceKey::new(NonZeroU64::new(1).unwrap());
        let domain_id = OwnershipDomainId::new(0).unwrap();
        let domain = OwnershipDomainCapability::new(
            domain_id,
            LogicalDeviceSelector::exact(vec![driver_key]).unwrap(),
            QueueExecution::Tagged,
            NonZeroU16::new(1).unwrap(),
            NonZeroU16::new(4).unwrap(),
            HardwareQueueDepth::fixed(NonZeroU16::new(4).unwrap()),
            irq_sources,
        )
        .unwrap();
        let device = LogicalDeviceCapability::new(
            driver_key,
            LogicalDeviceConstraints::discover_during_init(
                rdif_block::dma_api::DmaDomainId::legacy_global(),
                u64::MAX,
            ),
        );
        ValidationActivator {
            capabilities: ControllerCapabilities::new(
                NonZeroUsize::new(0x51).unwrap(),
                vec![device],
                vec![domain],
            )
            .unwrap(),
        }
    }

    fn validation_activator_with_independent_control(
        io_irq_sources: IdList,
        control_irq_sources: IdList,
    ) -> ValidationActivator {
        let mut activator = validation_activator(io_irq_sources);
        let control = ControlDomainCapability::independent(
            OwnershipDomainId::new(7).unwrap(),
            control_irq_sources,
        )
        .unwrap();
        activator.capabilities = ControllerCapabilities::new_with_control_capability(
            NonZeroUsize::new(0x51).unwrap(),
            control,
            activator.capabilities.logical_devices().to_vec(),
            activator.capabilities.domains().to_vec(),
        )
        .unwrap();
        activator
    }

    fn optional_domain_activator() -> ValidationActivator {
        let driver_key = DriverDeviceKey::new(NonZeroU64::new(1).unwrap());
        let selector = LogicalDeviceSelector::exact(vec![driver_key]).unwrap();
        let queue_depth = HardwareQueueDepth::fixed(NonZeroU16::new(4).unwrap());
        let required = OwnershipDomainCapability::new(
            OwnershipDomainId::new(0).unwrap(),
            selector.clone(),
            QueueExecution::Tagged,
            NonZeroU16::MIN,
            NonZeroU16::MIN,
            queue_depth,
            IdList::from_bits(1),
        )
        .unwrap();
        let optional = OwnershipDomainCapability::new_optional(
            OwnershipDomainId::new(1).unwrap(),
            selector,
            QueueExecution::Tagged,
            NonZeroU16::MIN,
            NonZeroU16::MIN,
            queue_depth,
            IdList::from_bits(1 << 1),
        )
        .unwrap();
        let device = LogicalDeviceCapability::new(
            driver_key,
            LogicalDeviceConstraints::discover_during_init(
                rdif_block::dma_api::DmaDomainId::legacy_global(),
                u64::MAX,
            ),
        );
        ValidationActivator {
            capabilities: ControllerCapabilities::new(
                NonZeroUsize::new(0x52).unwrap(),
                vec![device],
                vec![required, optional],
            )
            .unwrap(),
        }
    }

    fn pci_discovery_binding() -> BindingInfo {
        BindingInfo::empty().with_host_resources(
            BindingLocator::Pci {
                segment: 0,
                bus: 2,
                device: 3,
                function: 0,
            },
            vec![],
        )
    }
}
