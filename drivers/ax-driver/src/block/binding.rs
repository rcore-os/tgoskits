use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};

use ax_errno::AxError;
use log::warn;
use rdif_block::{
    BControllerBundle, BlkError, ControllerBundle, ControllerInitEndpoint, Interface,
    LifecycleKind, SingleDeviceBundle,
};
use rdrive::{Device, DeviceId, probe::OnProbeError};

use super::{IrqBoundBlock, IrqBoundControllerBundle};
use crate::{
    BindingInfo, BindingIrq, BindingLocator, HostMmioRange, IrqBindingLease,
    binding_info_from_acpi, binding_info_from_fdt,
    registration::{BoundDevice, register_bound_device},
};
#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci_endpoint};

/// Driver-core object retained by `rdrive` until an OS runtime takes ownership.
pub struct PlatformBlockDevice {
    name: String,
    bundle: Option<BControllerBundle>,
    binding: BlockDeviceBinding,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum BlockRegistrationError {
    #[error("interrupt-backed controller declares no IRQ source")]
    InterruptControllerWithoutIrqSource,
    #[error("controller declares IRQ source {source_id}, but the platform did not bind it")]
    MissingIrqBinding { source_id: usize },
    #[error("PCI block activation requires a retained move-only IRQ lease")]
    PciIrqLeaseRequired,
    #[error("deferred platform IRQ discovery facts must not contain realized IRQ sources")]
    DeferredBindingContainsIrqSource,
}

impl PlatformBlockDevice {
    fn new(
        device_id: DeviceId,
        name: String,
        bundle: BControllerBundle,
        binding: BindingInfo,
    ) -> Self {
        Self {
            name,
            bundle: Some(bundle),
            binding: BlockDeviceBinding::new(device_id, binding),
        }
    }
}

impl rdrive::DriverGeneric for PlatformBlockDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

impl BoundDevice for PlatformBlockDevice {
    fn binding_info(&self) -> &BindingInfo {
        &self.binding.platform
    }
}

/// Stable host identity and resource metadata retained for a block controller.
///
/// The `rdrive` device ID is the registry identity. The locator and MMIO ranges
/// allow the runtime to match an explicit passthrough request to this exact
/// controller without guessing from an interrupt number.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockDeviceBinding {
    device_id: DeviceId,
    platform: BindingInfo,
}

impl BlockDeviceBinding {
    pub(super) fn new(device_id: DeviceId, platform: BindingInfo) -> Self {
        Self {
            device_id,
            platform,
        }
    }

    /// Returns the stable `rdrive` registry identity.
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// Returns the firmware or bus locator captured at discovery time.
    pub const fn locator(&self) -> &BindingLocator {
        self.platform.locator()
    }

    /// Returns every validated host MMIO or PCI memory-BAR range.
    pub fn host_mmio_ranges(&self) -> &[HostMmioRange] {
        self.platform.host_mmio_ranges()
    }

    /// Returns all unresolved IRQ sources declared by the controller.
    pub fn irq_sources(&self) -> &[crate::BindingIrqBinding] {
        self.platform.irq_sources()
    }

    pub(super) const fn platform_binding(&self) -> &BindingInfo {
        &self.platform
    }

    /// Resolves one portable-driver IRQ source identity to its platform IRQ.
    ///
    /// `source_id` is the opaque source number declared by rdif-block, not an
    /// IRQ vector or an index into [`Self::irq_sources`]. This remains
    /// available after activation consumes the discovery owner because init
    /// may publish the final queue/source topology only at `Ready`.
    pub fn irq_for_source(&self, source_id: usize) -> Option<&BindingIrq> {
        self.platform.irq_for_source(source_id)
    }
}

/// A probed portable block controller and its unresolved platform IRQ bindings.
///
/// This type deliberately exposes no synchronous read/write wrapper. The OS
/// runtime must bind every declared interrupt source, validate each queue's
/// [`rdif_block::QueueKind`], and only then publish a filesystem-facing device.
pub struct RdifBlockDevice {
    name: String,
    binding: BlockDeviceBinding,
    bundle: BControllerBundle,
}

impl RdifBlockDevice {
    /// Returns the portable driver-reported name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the complete host resource binding retained for this controller.
    pub const fn binding(&self) -> &BlockDeviceBinding {
        &self.binding
    }

    /// Returns the stable `rdrive` registry identity.
    pub const fn device_id(&self) -> DeviceId {
        self.binding.device_id()
    }

    /// Returns the firmware or bus locator captured at discovery time.
    pub const fn locator(&self) -> &BindingLocator {
        self.binding.locator()
    }

    /// Returns every validated host MMIO or PCI memory-BAR range.
    pub fn host_mmio_ranges(&self) -> &[HostMmioRange] {
        self.binding.host_mmio_ranges()
    }

    pub fn irq_for_source(&self, source_id: usize) -> Option<&BindingIrq> {
        self.binding.irq_for_source(source_id)
    }

    pub fn irq_sources(&self) -> &[crate::BindingIrqBinding] {
        self.binding.irq_sources()
    }

    /// Returns the controller-wide portable ownership root.
    pub fn bundle(&self) -> &dyn ControllerBundle {
        &*self.bundle
    }

    /// Returns mutable access to controller initialization and extraction.
    pub fn bundle_mut(&mut self) -> &mut dyn ControllerBundle {
        &mut *self.bundle
    }

    /// Transfers the complete controller bundle to another runtime owner.
    pub fn into_bundle(self) -> BControllerBundle {
        self.bundle
    }

    pub fn enable_irq(&self) -> Result<(), BlkError> {
        self.bundle.enable_irq()
    }

    pub fn disable_irq(&self) -> Result<(), BlkError> {
        self.bundle.disable_irq()
    }
}

impl TryFrom<Device<PlatformBlockDevice>> for RdifBlockDevice {
    type Error = AxError;

    fn try_from(base: Device<PlatformBlockDevice>) -> Result<Self, Self::Error> {
        let mut device = base.lock().map_err(|_| AxError::BadState)?;
        let name = device.name.clone();
        let binding = device.binding.clone();
        let bundle = device.bundle.take().ok_or(AxError::BadState)?;
        Ok(Self {
            name,
            binding,
            bundle,
        })
    }
}

/// Transitional registration surface for legacy controller bundles.
///
/// New interrupt-backed controllers should implement
/// [`super::PlatformDeviceBlockActivation`] so queue topology and IRQ
/// ownership are selected through the v0.13 two-phase activation boundary.
pub trait PlatformDeviceBlock {
    fn register_block<T: Interface>(self, device: T) -> Option<usize>;

    fn register_block_with_info<T: Interface>(
        self,
        device: T,
        binding: BindingInfo,
    ) -> Option<usize>;

    /// Registers one controller that may expose several logical block devices.
    fn register_controller_bundle<T: ControllerBundle>(self, bundle: T) -> Option<usize>;

    /// Registers a controller bundle with its unresolved platform resources.
    fn register_controller_bundle_with_info<T: ControllerBundle>(
        self,
        bundle: T,
        binding: BindingInfo,
    ) -> Option<usize>;

    fn register_irq_bound_block<T, L>(self, device: T, irq_lease: L) -> Option<usize>
    where
        Self: Sized,
        T: Interface,
        L: IrqBindingLease;

    fn register_irq_bound_controller_bundle<T, L>(self, bundle: T, irq_lease: L) -> Option<usize>
    where
        Self: Sized,
        T: ControllerBundle,
        L: IrqBindingLease;
}

impl PlatformDeviceBlock for rdrive::PlatformDevice {
    fn register_block<T: Interface>(self, device: T) -> Option<usize> {
        self.register_block_with_info(device, BindingInfo::empty())
    }

    fn register_block_with_info<T: Interface>(
        self,
        device: T,
        binding: BindingInfo,
    ) -> Option<usize> {
        register_block_with_info(self, device, binding)
    }

    fn register_controller_bundle<T: ControllerBundle>(self, bundle: T) -> Option<usize> {
        self.register_controller_bundle_with_info(bundle, BindingInfo::empty())
    }

    fn register_controller_bundle_with_info<T: ControllerBundle>(
        self,
        bundle: T,
        binding: BindingInfo,
    ) -> Option<usize> {
        register_controller_bundle_with_info(self, bundle, binding)
    }

    fn register_irq_bound_block<T, L>(self, device: T, irq_lease: L) -> Option<usize>
    where
        T: Interface,
        L: IrqBindingLease,
    {
        let binding = irq_lease.binding_info();
        self.register_block_with_info(IrqBoundBlock::new(device, irq_lease), binding)
    }

    fn register_irq_bound_controller_bundle<T, L>(self, bundle: T, irq_lease: L) -> Option<usize>
    where
        T: ControllerBundle,
        L: IrqBindingLease,
    {
        let binding = irq_lease.binding_info();
        self.register_controller_bundle_with_info(
            IrqBoundControllerBundle::new(bundle, irq_lease),
            binding,
        )
    }
}

pub trait ProbeFdtBlock {
    fn register_block<T: Interface>(self, device: T) -> Result<Option<usize>, OnProbeError>;

    fn register_controller_bundle<T: ControllerBundle>(
        self,
        bundle: T,
    ) -> Result<Option<usize>, OnProbeError>;
}

impl ProbeFdtBlock for rdrive::probe::fdt::ProbeFdt<'_> {
    fn register_block<T: Interface>(self, mut device: T) -> Result<Option<usize>, OnProbeError> {
        let binding = binding_info_from_fdt(self.info())?;
        validate_block_interface_irq_bindings(&mut device, &binding)
            .map_err(block_registration_probe_error)?;
        Ok(register_block_with_info(
            self.into_platform_device(),
            device,
            binding,
        ))
    }

    fn register_controller_bundle<T: ControllerBundle>(
        self,
        mut bundle: T,
    ) -> Result<Option<usize>, OnProbeError> {
        let binding = binding_info_from_fdt(self.info())?;
        validate_controller_irq_bindings(&mut bundle, &binding)
            .map_err(block_registration_probe_error)?;
        Ok(register_controller_bundle_with_info(
            self.into_platform_device(),
            bundle,
            binding,
        ))
    }
}

pub trait ProbeAcpiBlock {
    fn register_block<T: Interface>(self, device: T) -> Result<Option<usize>, OnProbeError>;

    fn register_controller_bundle<T: ControllerBundle>(
        self,
        bundle: T,
    ) -> Result<Option<usize>, OnProbeError>;
}

impl ProbeAcpiBlock for rdrive::probe::acpi::ProbeAcpi<'_> {
    fn register_block<T: Interface>(self, mut device: T) -> Result<Option<usize>, OnProbeError> {
        let binding = binding_info_from_acpi(self.info())?;
        validate_block_interface_irq_bindings(&mut device, &binding)
            .map_err(block_registration_probe_error)?;
        Ok(register_block_with_info(
            self.into_platform_device(),
            device,
            binding,
        ))
    }

    fn register_controller_bundle<T: ControllerBundle>(
        self,
        mut bundle: T,
    ) -> Result<Option<usize>, OnProbeError> {
        let binding = binding_info_from_acpi(self.info())?;
        validate_controller_irq_bindings(&mut bundle, &binding)
            .map_err(block_registration_probe_error)?;
        Ok(register_controller_bundle_with_info(
            self.into_platform_device(),
            bundle,
            binding,
        ))
    }
}

#[cfg(feature = "pci")]
pub trait ProbePciBlock {
    fn register_block<T: Interface>(
        self,
        device: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError>;

    fn register_controller_bundle<T: ControllerBundle>(
        self,
        bundle: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError>;
}

#[cfg(feature = "pci")]
impl ProbePciBlock for rdrive::probe::pci::ProbePci<'_> {
    fn register_block<T: Interface>(
        self,
        mut device: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError> {
        let binding = binding_info_from_pci_endpoint(self.info(), self.endpoint(), requirement)?;
        validate_block_interface_irq_bindings(&mut device, &binding)
            .map_err(block_registration_probe_error)?;
        Ok(register_block_with_info(
            self.into_platform_device(),
            device,
            binding,
        ))
    }

    fn register_controller_bundle<T: ControllerBundle>(
        self,
        mut bundle: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError> {
        let binding = binding_info_from_pci_endpoint(self.info(), self.endpoint(), requirement)?;
        validate_controller_irq_bindings(&mut bundle, &binding)
            .map_err(block_registration_probe_error)?;
        Ok(register_controller_bundle_with_info(
            self.into_platform_device(),
            bundle,
            binding,
        ))
    }
}

fn register_block_with_info<T: Interface>(
    platform: rdrive::PlatformDevice,
    device: T,
    binding: BindingInfo,
) -> Option<usize> {
    register_controller_bundle_with_info(
        platform,
        SingleDeviceBundle::new(Box::new(device)),
        binding,
    )
}

fn register_controller_bundle_with_info<T: ControllerBundle>(
    platform: rdrive::PlatformDevice,
    mut bundle: T,
    binding: BindingInfo,
) -> Option<usize> {
    if let Err(error) = validate_controller_irq_bindings(&mut bundle, &binding) {
        warn!(
            "refusing to register block controller {}: {error}",
            bundle.name()
        );
        return None;
    }
    let name = bundle.name().to_string();
    let device_id = platform.descriptor().device_id();
    register_bound_device(
        platform,
        PlatformBlockDevice::new(device_id, name, Box::new(bundle), binding),
    )
}

fn validate_controller_irq_bindings(
    bundle: &mut dyn ControllerBundle,
    binding: &BindingInfo,
) -> Result<(), BlockRegistrationError> {
    let lifecycle = bundle.lifecycle().kind();
    let mut required_sources = Vec::new();
    for source in bundle.irq_sources() {
        if !required_sources.contains(&source.id) {
            required_sources.push(source.id);
        }
    }
    if let ControllerInitEndpoint::Pending(initializer) = bundle.controller_init() {
        for source_id in initializer.irq_sources().iter() {
            if !required_sources.contains(&source_id) {
                required_sources.push(source_id);
            }
        }
    }
    validate_required_irq_bindings(lifecycle, &required_sources, binding)
}

fn validate_required_irq_bindings(
    lifecycle: LifecycleKind,
    required_sources: &[usize],
    binding: &BindingInfo,
) -> Result<(), BlockRegistrationError> {
    if lifecycle == LifecycleKind::Interrupt && required_sources.is_empty() {
        return Err(BlockRegistrationError::InterruptControllerWithoutIrqSource);
    }
    for &source_id in required_sources {
        if binding.irq_for_source(source_id).is_none() {
            return Err(BlockRegistrationError::MissingIrqBinding { source_id });
        }
    }
    Ok(())
}

pub(crate) fn validate_block_interface_irq_bindings(
    interface: &mut dyn Interface,
    binding: &BindingInfo,
) -> Result<(), BlockRegistrationError> {
    let lifecycle = interface.lifecycle().kind();
    let mut required_sources = Vec::new();
    for source in interface.irq_sources() {
        if !required_sources.contains(&source.id) {
            required_sources.push(source.id);
        }
    }
    if let ControllerInitEndpoint::Pending(initializer) = interface.controller_init() {
        for source_id in initializer.irq_sources().iter() {
            if !required_sources.contains(&source_id) {
                required_sources.push(source_id);
            }
        }
    }
    validate_required_irq_bindings(lifecycle, &required_sources, binding)
}

fn block_registration_probe_error(error: BlockRegistrationError) -> OnProbeError {
    OnProbeError::other(error.to_string())
}

/// Transfers every discovered legacy block controller to the OS block runtime.
///
/// This transitional collector cannot observe or consume v0.13 controller
/// activators. Use [`super::take_rdif_block_activators`] for that boundary.
pub fn take_rdif_block_devices() -> Vec<RdifBlockDevice> {
    rdrive::get_list::<PlatformBlockDevice>()
        .into_iter()
        .filter_map(|device| match RdifBlockDevice::try_from(device) {
            Ok(block) => Some(block),
            Err(error) => {
                warn!("failed to take RDIF block device: {error:?}");
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use alloc::{string::String, vec};

    use rdif_block::{
        BlkError, BlockIrqSource, ControllerInitEndpoint, DeviceInfo, DriverGeneric, IdList,
        InitInput, InitPoll, InitialController, Interface, IrqSourceInfo, IrqSourceList,
        LifecycleEndpoint, QueueHandle, QueueLimits,
    };

    use super::*;

    struct ValidationInitializer {
        sources: IdList,
    }

    impl InitialController for ValidationInitializer {
        fn irq_sources(&self) -> IdList {
            self.sources
        }

        fn take_irq_source(&mut self, _source_id: usize) -> Option<BlockIrqSource> {
            None
        }

        fn poll_init(&mut self, _input: InitInput) -> InitPoll<()> {
            InitPoll::Ready(())
        }
    }

    struct ValidationBlock {
        declared: IrqSourceList,
        initializer: ValidationInitializer,
    }

    impl DriverGeneric for ValidationBlock {
        fn name(&self) -> &str {
            "validation-block"
        }
    }

    impl Interface for ValidationBlock {
        fn controller_init(&mut self) -> ControllerInitEndpoint<'_> {
            ControllerInitEndpoint::Pending(&mut self.initializer)
        }

        fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
            LifecycleEndpoint::Inline
        }

        fn device_info(&self) -> DeviceInfo {
            DeviceInfo::new(1, 512)
        }

        fn queue_limits(&self) -> QueueLimits {
            QueueLimits::simple(512, u64::MAX)
        }

        fn create_queue(&mut self) -> Option<QueueHandle> {
            None
        }

        fn enable_irq(&self) -> Result<(), BlkError> {
            Ok(())
        }

        fn disable_irq(&self) -> Result<(), BlkError> {
            Ok(())
        }

        fn is_irq_enabled(&self) -> bool {
            false
        }

        fn irq_sources(&self) -> IrqSourceList {
            self.declared.clone()
        }

        fn take_irq_source(&mut self, _source_id: usize) -> Option<BlockIrqSource> {
            None
        }
    }

    #[test]
    fn block_binding_retains_registry_identity_locator_and_mmio_ranges() {
        let device_id = DeviceId::from(41);
        let range = HostMmioRange::try_new(0x8000_0000, 0x2000).unwrap();
        let platform = BindingInfo::empty().with_host_resources(
            BindingLocator::Fdt {
                path: String::from("/soc/storage@80000000"),
            },
            vec![range],
        );

        let binding = BlockDeviceBinding::new(device_id, platform);

        assert_eq!(binding.device_id(), device_id);
        assert_eq!(
            binding.locator(),
            &BindingLocator::Fdt {
                path: String::from("/soc/storage@80000000"),
            }
        );
        assert_eq!(binding.host_mmio_ranges(), &[range]);
    }

    #[test]
    fn registration_requires_normal_and_initialization_irq_bindings() {
        let mut init_sources = IdList::none();
        init_sources.insert(7);
        let mut block = ValidationBlock {
            declared: vec![IrqSourceInfo::new(3, IdList::from_bits(1))],
            initializer: ValidationInitializer {
                sources: init_sources,
            },
        };

        assert_eq!(
            validate_block_interface_irq_bindings(&mut block, &BindingInfo::empty()),
            Err(BlockRegistrationError::MissingIrqBinding { source_id: 3 })
        );

        let normal_only = BindingInfo::with_irq_sources([(3, BindingIrq::acpi_gsi(19))]);
        assert_eq!(
            validate_block_interface_irq_bindings(&mut block, &normal_only),
            Err(BlockRegistrationError::MissingIrqBinding { source_id: 7 })
        );

        let all_sources = BindingInfo::with_irq_sources([
            (3, BindingIrq::acpi_gsi(19)),
            (7, BindingIrq::acpi_gsi(20)),
        ]);
        assert_eq!(
            validate_block_interface_irq_bindings(&mut block, &all_sources),
            Ok(())
        );
    }

    #[test]
    fn interrupt_backed_controller_without_any_irq_source_fails_closed() {
        assert_eq!(
            validate_required_irq_bindings(LifecycleKind::Interrupt, &[], &BindingInfo::empty()),
            Err(BlockRegistrationError::InterruptControllerWithoutIrqSource)
        );
        assert_eq!(
            validate_required_irq_bindings(LifecycleKind::Inline, &[], &BindingInfo::empty()),
            Ok(())
        );
    }
}
