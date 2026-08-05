//! Version-neutral VGIC state owner and wired-controller capability.

use alloc::{collections::BTreeMap, sync::Arc};

use ax_kspin::SpinRaw;
use axdevice_base::{
    ControllerInputId, InterruptControllerId, InterruptEndpoint, InterruptTrigger, IrqError,
    IrqResult, ItsId, LpiId as EndpointLpiId, MessageInterruptController, MessageInterruptSink,
    MsiDeviceId, MsiEndpoint, MsiEventId, MsiMessage, VirtualInterruptController, WiredIrqInput,
    WiredIrqSink,
};
use axvm_types::AccessWidth;

use crate::{
    ArmVgicConfig, EventId, GicV3Backend, GicV3VcpuBinding, GicV3VcpuWake, GicVcpuId, GuestMemory,
    HostGicVersion, IntId, ItsDeviceId, LpiId, PhysicalIrqId, SpiId, TriggerMode, VgicController,
    VgicError, VgicResult,
};

/// The single canonical virtual interrupt-controller state owner for one VM.
pub struct VgicCore {
    config: ArmVgicConfig,
    controller: VgicController,
    inputs: SpinRaw<BTreeMap<ControllerInputId, WiredIrqInput>>,
    sink: Arc<VgicWiredSink>,
    message_sink: Arc<VgicMessageSink>,
}

impl VgicCore {
    /// Creates a VGIC without a guest-memory ITS capability.
    pub fn new(config: ArmVgicConfig, backend: Arc<dyn GicV3Backend>) -> VgicResult<Self> {
        Self::new_with_guest_memory(config, backend, None)
    }

    /// Creates a VGIC with checked guest-memory access for an optional ITS.
    pub fn new_with_guest_memory(
        config: ArmVgicConfig,
        backend: Arc<dyn GicV3Backend>,
        guest_memory: Option<Arc<dyn GuestMemory>>,
    ) -> VgicResult<Self> {
        validate_backend_capabilities(&config, backend.capabilities())?;
        let id = config.controller_id();
        let controller = VgicController::new_from_arm_config(&config, backend, guest_memory)?;
        Ok(Self {
            config,
            sink: Arc::new(VgicWiredSink {
                controller: controller.clone(),
                id,
            }),
            message_sink: Arc::new(VgicMessageSink {
                controller: controller.clone(),
                id,
            }),
            controller,
            inputs: SpinRaw::new(BTreeMap::new()),
        })
    }

    /// Returns the immutable configuration used for construction and firmware.
    pub const fn config(&self) -> &ArmVgicConfig {
        &self.config
    }

    /// Returns the VM-local controller identifier.
    pub const fn id(&self) -> InterruptControllerId {
        self.config.controller_id()
    }

    /// Returns the underlying canonical controller for architecture frontends.
    pub const fn controller(&self) -> &VgicController {
        &self.controller
    }

    /// Reads the configured GICv2 Distributor frontend.
    pub fn read_v2_distributor(
        &self,
        vcpu: GicVcpuId,
        offset: u64,
        width: AccessWidth,
    ) -> VgicResult<u64> {
        self.require_v2("read GICv2 Distributor")?;
        self.controller.read_v2_distributor(vcpu, offset, width)
    }

    /// Writes the configured GICv2 Distributor frontend.
    pub fn write_v2_distributor(
        &self,
        vcpu: GicVcpuId,
        offset: u64,
        width: AccessWidth,
        value: u64,
    ) -> VgicResult {
        self.require_v2("write GICv2 Distributor")?;
        self.controller
            .write_v2_distributor(vcpu, offset, width, value)
    }

    /// Reads the configured GICv2 CPU-interface frontend.
    pub fn read_v2_cpu_interface(
        &self,
        vcpu: GicVcpuId,
        offset: u64,
        width: AccessWidth,
    ) -> VgicResult<u64> {
        self.require_v2("read GICv2 CPU interface")?;
        self.controller.read_v2_cpu_interface(vcpu, offset, width)
    }

    /// Writes the configured GICv2 CPU-interface frontend.
    pub fn write_v2_cpu_interface(
        &self,
        vcpu: GicVcpuId,
        offset: u64,
        width: AccessWidth,
        value: u64,
    ) -> VgicResult {
        self.require_v2("write GICv2 CPU interface")?;
        self.controller
            .write_v2_cpu_interface(vcpu, offset, width, value)
    }

    /// Attaches one configured vCPU.
    pub fn attach_vcpu(
        &self,
        vcpu: usize,
        wake: Arc<dyn GicV3VcpuWake>,
    ) -> VgicResult<GicV3VcpuBinding> {
        let affinity = self
            .config
            .vcpu_affinities()
            .get(vcpu)
            .copied()
            .ok_or_else(|| crate::VgicError::ResourceNotFound {
                resource: alloc::format!("configured vCPU {vcpu}"),
                operation: "attach VGIC vCPU",
            })?;
        self.controller
            .attach_vcpu(GicVcpuId::new(vcpu), affinity, wake)
    }

    /// Installs every physical SPI fixed by the immutable configuration.
    pub fn bind_assigned_spis(&self) -> VgicResult {
        let mut bound = alloc::vec::Vec::with_capacity(self.config.assigned_spis().len());
        for binding in self.config.assigned_spis() {
            if let Err(error) = self.controller.bind_physical_spi_with_trigger(
                binding.intid(),
                PhysicalIrqId::new(binding.host_irq().value() as u64),
                GicVcpuId::new(binding.target_vcpu()),
                binding.trigger(),
            ) {
                for spi in bound.into_iter().rev() {
                    if let Err(rollback_error) = self.controller.unbind_physical_spi(spi) {
                        log::warn!(
                            "failed to roll back assigned physical SPI {}: {rollback_error}",
                            spi.raw()
                        );
                    }
                }
                return Err(error);
            }
            bound.push(binding.intid());
        }
        Ok(())
    }

    /// Releases every quiescent physical SPI fixed by the configuration.
    ///
    /// The method attempts every binding so one backend failure does not leak
    /// unrelated host resources. The first failure is returned after the
    /// remaining releases have been attempted.
    pub fn unbind_assigned_spis(&self) -> VgicResult {
        let mut first_error = None;
        for binding in self.config.assigned_spis().iter().rev() {
            if let Err(error) = self.controller.unbind_physical_spi(binding.intid())
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Retires and releases every assigned SPI while tearing down a stopped VM.
    ///
    /// All bindings are attempted so one backend failure does not retain
    /// unrelated host resources. Failed bindings keep their controller claim
    /// and may be retried without deactivating an already retired delivery.
    pub fn teardown_assigned_spis(&self) -> VgicResult {
        let mut first_error = None;
        for binding in self.config.assigned_spis().iter().rev() {
            if let Err(error) = self.controller.teardown_physical_spi(binding.intid())
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Forwards one acknowledged identity-backed host SPI.
    pub fn forward_physical_spi(&self, host_irq: axdevice_base::HostIrqId) -> VgicResult {
        self.controller
            .forward_physical_spi(SpiId::new(host_irq.value() as u32)?)
    }

    /// Injects one private or shared interrupt through canonical state.
    pub fn inject(&self, vcpu: usize, intid: u32, trigger: InterruptTrigger) -> VgicResult {
        match IntId::new(intid)? {
            IntId::Sgi(sgi) => {
                self.controller
                    .send_sgi(GicVcpuId::new(vcpu), sgi, crate::SgiTarget::SelfOnly)
            }
            IntId::Ppi(ppi) => match trigger {
                InterruptTrigger::EdgeTriggered => {
                    self.controller.pulse_ppi(GicVcpuId::new(vcpu), ppi)
                }
                InterruptTrigger::LevelTriggered => {
                    self.controller
                        .set_ppi_level(GicVcpuId::new(vcpu), ppi, true)
                }
            },
            IntId::Spi(spi) => match trigger {
                InterruptTrigger::EdgeTriggered => self.controller.pulse_spi(spi),
                InterruptTrigger::LevelTriggered => self.controller.set_spi_level(spi, true),
            },
            IntId::Lpi(_) => Err(crate::VgicError::Unsupported {
                operation: "inject wired interrupt",
                detail: "LPIs must be delivered through an ITS endpoint".into(),
            }),
        }
    }

    fn open_input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTrigger,
    ) -> IrqResult<WiredIrqInput> {
        if let Some(existing) = self.inputs.lock().get(&input).cloned() {
            return validate_existing_input(existing, trigger);
        }
        let spi = SpiId::new(input.value() as u32)
            .map_err(|error| irq_backend_error(self.id(), input, "open SPI input", error))?;
        self.controller
            .configure_spi_input(spi, trigger_mode(trigger))
            .map_err(|error| irq_backend_error(self.id(), input, "configure SPI input", error))?;
        let opened = WiredIrqInput::new(self.id(), input, trigger, self.sink.clone());
        let mut inputs = self.inputs.lock();
        if let Some(existing) = inputs.get(&input).cloned() {
            return validate_existing_input(existing, trigger);
        }
        inputs.insert(input, opened.clone());
        Ok(opened)
    }

    fn require_v2(&self, operation: &'static str) -> VgicResult {
        if matches!(self.config, ArmVgicConfig::V2(_)) {
            Ok(())
        } else {
            Err(VgicError::Unsupported {
                operation,
                detail: "the immutable controller configuration selects GICv3".into(),
            })
        }
    }
}

fn validate_backend_capabilities(
    config: &ArmVgicConfig,
    capabilities: crate::VgicBackendCapabilities,
) -> VgicResult {
    let required_version = match config {
        ArmVgicConfig::V2(_) => HostGicVersion::V2,
        ArmVgicConfig::V3(_) => HostGicVersion::V3,
    };
    if capabilities.host_version() != required_version {
        return Err(VgicError::Unsupported {
            operation: "create VGIC",
            detail: alloc::format!(
                "guest {:?} does not match host {:?}",
                required_version,
                capabilities.host_version()
            ),
        });
    }
    if config.list_register_count() > capabilities.list_register_count() {
        return Err(VgicError::InvalidConfig {
            detail: alloc::format!(
                "configuration requests {} LRs, host exposes {}",
                config.list_register_count(),
                capabilities.list_register_count()
            ),
        });
    }
    if config.priority_bits() > capabilities.priority_bits() {
        return Err(VgicError::InvalidConfig {
            detail: alloc::format!(
                "configuration requests {} priority bits, host exposes {}",
                config.priority_bits(),
                capabilities.priority_bits()
            ),
        });
    }
    Ok(())
}

impl core::fmt::Debug for VgicCore {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("VgicCore")
            .field("config", &self.config)
            .field("opened_inputs", &self.inputs.lock().len())
            .finish_non_exhaustive()
    }
}

impl VirtualInterruptController for VgicCore {
    fn id(&self) -> InterruptControllerId {
        self.config.controller_id()
    }

    fn wired_input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTrigger,
    ) -> IrqResult<WiredIrqInput> {
        self.open_input(input, trigger)
    }
}

impl MessageInterruptController for VgicCore {
    fn id(&self) -> InterruptControllerId {
        self.config.controller_id()
    }

    fn msi_endpoint(
        &self,
        its: ItsId,
        device: MsiDeviceId,
        event: MsiEventId,
        lpi: EndpointLpiId,
    ) -> IrqResult<MsiEndpoint> {
        let reserved_lpi = LpiId::new(lpi.value()).map_err(|error| {
            msi_backend_error(
                self.id(),
                MsiMessage::new(its, device, event, lpi),
                "validate planned LPI",
                error,
            )
        })?;
        self.controller
            .configure_msi_input_for(
                its,
                ItsDeviceId::new(device.value()),
                EventId::new(event.value()),
                Some(reserved_lpi),
            )
            .map_err(|error| {
                msi_backend_error(
                    self.id(),
                    MsiMessage::new(its, device, event, lpi),
                    "open MSI endpoint",
                    error,
                )
            })?;
        Ok(MsiEndpoint::new(
            self.id(),
            MsiMessage::new(its, device, event, lpi),
            self.message_sink.clone(),
        ))
    }
}

struct VgicWiredSink {
    controller: VgicController,
    id: InterruptControllerId,
}

impl WiredIrqSink for VgicWiredSink {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        let spi = SpiId::new(input.value() as u32)
            .map_err(|error| irq_backend_error(self.id, input, "set SPI level", error))?;
        self.controller
            .set_spi_level(spi, asserted)
            .map_err(|error| irq_backend_error(self.id, input, "set SPI level", error))
    }

    fn pulse(&self, input: ControllerInputId) -> IrqResult {
        let spi = SpiId::new(input.value() as u32)
            .map_err(|error| irq_backend_error(self.id, input, "pulse SPI", error))?;
        self.controller
            .pulse_spi(spi)
            .map_err(|error| irq_backend_error(self.id, input, "pulse SPI", error))
    }
}

struct VgicMessageSink {
    controller: VgicController,
    id: InterruptControllerId,
}

impl MessageInterruptSink for VgicMessageSink {
    fn signal(&self, message: MsiMessage) -> IrqResult {
        self.controller
            .signal_msi_for(
                message.its(),
                ItsDeviceId::new(message.device().value()),
                EventId::new(message.event().value()),
            )
            .map_err(|error| msi_backend_error(self.id, message, "signal MSI", error))
    }
}

pub(crate) fn trigger_mode(trigger: InterruptTrigger) -> TriggerMode {
    match trigger {
        InterruptTrigger::EdgeTriggered => TriggerMode::Edge,
        InterruptTrigger::LevelTriggered => TriggerMode::Level,
    }
}

fn validate_existing_input(
    existing: WiredIrqInput,
    trigger: InterruptTrigger,
) -> IrqResult<WiredIrqInput> {
    if existing.trigger() != trigger {
        return Err(IrqError::InvalidTriggerMode {
            endpoint: InterruptEndpoint::Wired {
                controller: existing.controller(),
                input: existing.input(),
            },
            operation: "open VGIC input",
            expected: existing.trigger(),
            actual: trigger,
        });
    }
    Ok(existing)
}

fn irq_backend_error(
    controller: InterruptControllerId,
    input: ControllerInputId,
    operation: &'static str,
    error: crate::VgicError,
) -> IrqError {
    IrqError::Backend {
        endpoint: InterruptEndpoint::Wired { controller, input },
        operation,
        detail: alloc::format!("{error}"),
    }
}

fn msi_backend_error(
    controller: InterruptControllerId,
    message: MsiMessage,
    operation: &'static str,
    error: crate::VgicError,
) -> IrqError {
    IrqError::Backend {
        endpoint: InterruptEndpoint::Message {
            controller,
            its: message.its(),
            device: message.device(),
            event: message.event(),
            lpi: message.lpi(),
        },
        operation,
        detail: alloc::format!("{error}"),
    }
}
