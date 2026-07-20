//! Host SPI ownership and forwarding into one VM-local GICv3.

use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use arm_vgic::{GicV3BackendError, GicV3Controller, GicVcpuId, SpiId};
use ax_kspin::SpinRaw;
use ax_std::os::arceos::modules::ax_hal::irq::{self as host_irq, IrqHandle, IrqReturn};
use axdevice::{
    ControllerInputId, InterruptControllerId, InterruptEndpointRegistration,
    InterruptPlanAuthority, InterruptSharing, InterruptTopology, IrqLine, WiredIrqRequest,
};
use axvm_types::InterruptTriggerMode;

use super::{AxvmGicV3Backend, physical_spi::resolve_host_irq};
use crate::{AxVmError, AxVmResult, machine::HostInterruptResource};

/// VM-owned host IRQ actions for mediated or hardware-backed delivery.
pub(crate) struct HostSpiForwarding {
    backend: Arc<AxvmGicV3Backend>,
    spis: Vec<Arc<ForwardedSpi>>,
}

impl HostSpiForwarding {
    /// Connects assigned SPIs through VM-local topology lines.
    pub(crate) fn connect_mediated(
        topology: &InterruptTopology,
        authority: &InterruptPlanAuthority,
        controller: InterruptControllerId,
        interrupts: &[HostInterruptResource],
        backend: Arc<AxvmGicV3Backend>,
    ) -> AxVmResult<Self> {
        let mut forwarding = Self::new(interrupts.len(), backend);
        for interrupt in interrupts {
            forwarding.connect_mediated_spi(topology, authority, controller, interrupt)?;
        }
        Ok(forwarding)
    }

    /// Connects assigned SPIs as exclusive physical sources for HW LRs.
    pub(crate) fn connect_hardware_backed(
        topology: &InterruptTopology,
        authority: &InterruptPlanAuthority,
        controller_id: InterruptControllerId,
        controller: Arc<GicV3Controller>,
        target: GicVcpuId,
        interrupts: &[HostInterruptResource],
        backend: Arc<AxvmGicV3Backend>,
    ) -> AxVmResult<Self> {
        let mut forwarding = Self::new(interrupts.len(), backend);
        for interrupt in interrupts {
            forwarding.connect_hardware_backed_spi(
                topology,
                authority,
                controller_id,
                controller.clone(),
                target,
                interrupt,
            )?;
        }
        Ok(forwarding)
    }

    fn new(capacity: usize, backend: Arc<AxvmGicV3Backend>) -> Self {
        Self {
            backend,
            spis: Vec::with_capacity(capacity),
        }
    }

    fn connect_mediated_spi(
        &mut self,
        topology: &InterruptTopology,
        authority: &InterruptPlanAuthority,
        controller: InterruptControllerId,
        interrupt: &HostInterruptResource,
    ) -> AxVmResult {
        let intid = interrupt.input_u32();
        let spi = validate_spi(intid, "validate mediated GICv3 SPI")?;
        let input = usize::try_from(intid).map_err(|_| {
            AxVmError::invalid_config("mediated GICv3 SPI INTID does not fit usize")
        })?;
        let claim = authority.claim_wired(
            topology,
            WiredIrqRequest::for_controller(
                controller,
                ControllerInputId::new(input),
                interrupt.trigger(),
                InterruptSharing::Exclusive,
            ),
        )?;
        let (line, endpoint_registration) = topology.connect_irq(claim)?.into_parts();
        let irq = resolve_host_irq(intid)
            .map_err(|error| AxVmError::interrupt("resolve mediated GICv3 host SPI", error))?;
        let forwarding = Arc::new(ForwardedSpi::mediated(
            spi,
            irq,
            line,
            endpoint_registration,
        ));
        let handler = forwarding.clone();
        let request = host_irq::IrqRequest::new(move |_| handler.handle_host_irq())
            .share_mode(host_irq::ShareMode::Shared)
            .auto_enable(host_irq::AutoEnable::No);
        let registration = request_host_irq(irq, intid, "mediated", request)?;
        forwarding.install_registration(registration)?;
        self.spis.push(forwarding.clone());
        self.backend
            .register_emulated_spi(spi, Arc::downgrade(&forwarding))
            .map_err(|error| AxVmError::interrupt("register mediated GICv3 retirement", error))?;
        host_irq::enable_irq(registration).map_err(|error| {
            AxVmError::interrupt(
                "enable mediated GICv3 host SPI forwarding",
                alloc::format!("host IRQ {irq:?}, guest INTID {intid}: {error:?}"),
            )
        })
    }

    fn connect_hardware_backed_spi(
        &mut self,
        topology: &InterruptTopology,
        authority: &InterruptPlanAuthority,
        controller_id: InterruptControllerId,
        controller: Arc<GicV3Controller>,
        target: GicVcpuId,
        interrupt: &HostInterruptResource,
    ) -> AxVmResult {
        let intid = interrupt.input_u32();
        let spi = validate_spi(intid, "validate hardware-backed GICv3 SPI")?;
        let input = usize::try_from(intid).map_err(|_| {
            AxVmError::invalid_config("hardware-backed GICv3 SPI INTID does not fit usize")
        })?;
        let claim = authority.claim_wired(
            topology,
            WiredIrqRequest::for_controller(
                controller_id,
                ControllerInputId::new(input),
                interrupt.trigger(),
                InterruptSharing::Exclusive,
            ),
        )?;
        let endpoint_registration = topology.authorize_wired_endpoint(claim)?;
        let irq = resolve_host_irq(intid).map_err(|error| {
            AxVmError::interrupt("resolve hardware-backed GICv3 host SPI", error)
        })?;
        let route = self.backend.route(target).map_err(|error| {
            AxVmError::interrupt("resolve hardware-backed GICv3 vCPU route", error)
        })?;
        let forwarding = Arc::new(ForwardedSpi::hardware_backed(
            spi,
            irq,
            controller,
            endpoint_registration,
        ));
        let handler = forwarding.clone();
        let request = host_irq::IrqRequest::new(move |_| handler.handle_host_irq())
            .share_mode(host_irq::ShareMode::Exclusive)
            .affinity(host_irq::IrqAffinity::Fixed(host_irq::CpuId(
                route.host_cpu,
            )))
            .auto_enable(host_irq::AutoEnable::No);
        let registration = request_host_irq(irq, intid, "hardware-backed", request)?;
        forwarding.install_registration(registration)?;
        self.spis.push(forwarding.clone());
        // `request_irq` preserves pre-existing line state. A hardware-backed assignment
        // must remain masked until the guest enables its owned Distributor bit.
        host_irq::disable_irq(registration).map_err(|error| {
            AxVmError::interrupt(
                "mask newly assigned hardware-backed GICv3 SPI",
                alloc::format!("host IRQ {irq:?}, guest INTID {intid}: {error:?}"),
            )
        })?;
        self.backend
            .register_hardware_backed_spi(spi, Arc::downgrade(&forwarding))
            .map_err(|error| {
                AxVmError::interrupt("register hardware-backed GICv3 forwarding", error)
            })
    }
}

impl Drop for HostSpiForwarding {
    fn drop(&mut self) {
        for spi in self.spis.drain(..).rev() {
            match spi.target {
                ForwardingTarget::Mediated(_) => self.backend.unregister_emulated_spi(spi.spi()),
                ForwardingTarget::HardwareBacked(_) => {
                    self.backend.unregister_hardware_backed_spi(spi.spi())
                }
            }
            if let Err(error) = spi.release_registration() {
                warn!(
                    "failed to release GICv3 host SPI forwarding for {:?}: {error:?}",
                    spi.host_irq()
                );
            }
        }
    }
}

enum ForwardingTarget {
    Mediated(IrqLine),
    HardwareBacked(Arc<GicV3Controller>),
}

pub(super) struct ForwardedSpi {
    spi: SpiId,
    host_irq: host_irq::IrqId,
    target: ForwardingTarget,
    _endpoint_registration: Option<InterruptEndpointRegistration>,
    registration: SpinRaw<Option<IrqHandle>>,
    host_masked: AtomicBool,
}

impl ForwardedSpi {
    fn mediated(
        spi: SpiId,
        host_irq: host_irq::IrqId,
        line: IrqLine,
        endpoint_registration: InterruptEndpointRegistration,
    ) -> Self {
        Self {
            spi,
            host_irq,
            target: ForwardingTarget::Mediated(line),
            _endpoint_registration: Some(endpoint_registration),
            registration: SpinRaw::new(None),
            host_masked: AtomicBool::new(false),
        }
    }

    fn hardware_backed(
        spi: SpiId,
        host_irq: host_irq::IrqId,
        controller: Arc<GicV3Controller>,
        endpoint_registration: InterruptEndpointRegistration,
    ) -> Self {
        Self {
            spi,
            host_irq,
            target: ForwardingTarget::HardwareBacked(controller),
            _endpoint_registration: Some(endpoint_registration),
            registration: SpinRaw::new(None),
            host_masked: AtomicBool::new(false),
        }
    }

    const fn spi(&self) -> SpiId {
        self.spi
    }

    const fn host_irq(&self) -> host_irq::IrqId {
        self.host_irq
    }

    fn install_registration(&self, registration: IrqHandle) -> AxVmResult {
        let mut slot = self.registration.lock();
        if slot.is_some() {
            return Err(AxVmError::invalid_config(alloc::format!(
                "host IRQ {:?} already has a GICv3 forwarding action",
                self.host_irq
            )));
        }
        *slot = Some(registration);
        Ok(())
    }

    fn handle_host_irq(&self) -> IrqReturn {
        match &self.target {
            ForwardingTarget::Mediated(line) => self.handle_mediated_irq(line),
            ForwardingTarget::HardwareBacked(controller) => {
                match controller.forward_physical_spi(self.spi) {
                    Ok(()) => IrqReturn::Forwarded,
                    Err(error) => {
                        warn!(
                            "failed to forward hardware-backed host IRQ {:?} to guest SPI {}: \
                             {error}",
                            self.host_irq,
                            self.spi.raw()
                        );
                        IrqReturn::Unhandled
                    }
                }
            }
        }
    }

    fn handle_mediated_irq(&self, line: &IrqLine) -> IrqReturn {
        if self
            .host_masked
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return IrqReturn::Handled;
        }
        let Some(registration) = *self.registration.lock() else {
            self.host_masked.store(false, Ordering::Release);
            warn!(
                "host IRQ {:?} arrived before its mediated forwarding action was installed",
                self.host_irq
            );
            return IrqReturn::Unhandled;
        };
        if let Err(error) = host_irq::disable_irq(registration) {
            self.host_masked.store(false, Ordering::Release);
            warn!(
                "failed to mask host IRQ {:?} before forwarding guest SPI {}: {error:?}",
                self.host_irq,
                self.spi.raw()
            );
            return IrqReturn::Unhandled;
        }
        let signal_result = match line.trigger() {
            InterruptTriggerMode::LevelTriggered => line.raise(),
            InterruptTriggerMode::EdgeTriggered => line.pulse(),
        };
        match signal_result {
            Ok(()) => IrqReturn::Handled,
            Err(error) => {
                self.host_masked.store(false, Ordering::Release);
                if let Err(enable_error) = host_irq::enable_irq(registration) {
                    warn!(
                        "failed to restore host IRQ {:?} after forwarding guest SPI {} failed: \
                         {enable_error:?}",
                        self.host_irq,
                        self.spi.raw()
                    );
                }
                warn!(
                    "failed to forward host IRQ {:?} to guest SPI {}: {error}",
                    self.host_irq,
                    self.spi.raw()
                );
                IrqReturn::Unhandled
            }
        }
    }

    pub(super) fn set_hardware_backed_enabled(
        &self,
        enabled: bool,
    ) -> Result<(), GicV3BackendError> {
        if !matches!(self.target, ForwardingTarget::HardwareBacked(_)) {
            return Err(GicV3BackendError::new(
                "set hardware-backed SPI forwarding state",
                alloc::format!("guest SPI {} is mediated", self.spi.raw()),
            ));
        }
        let registration = (*self.registration.lock()).ok_or_else(|| {
            GicV3BackendError::new(
                "set hardware-backed SPI forwarding state",
                alloc::format!("guest SPI {} has no host IRQ action", self.spi.raw()),
            )
        })?;
        let result = if enabled {
            host_irq::enable_irq(registration)
        } else {
            host_irq::disable_irq(registration)
        };
        result.map_err(|error| {
            GicV3BackendError::new(
                "set hardware-backed SPI forwarding state",
                alloc::format!(
                    "host IRQ {:?}, guest SPI {}: {error:?}",
                    self.host_irq,
                    self.spi.raw()
                ),
            )
        })
    }

    pub(super) fn retire_guest_interrupt(&self) -> Result<(), GicV3BackendError> {
        let ForwardingTarget::Mediated(line) = &self.target else {
            return Err(GicV3BackendError::new(
                "retire mediated SPI",
                alloc::format!("guest SPI {} is hardware-backed", self.spi.raw()),
            ));
        };
        if line.trigger() == InterruptTriggerMode::LevelTriggered {
            line.lower().map_err(|error| {
                GicV3BackendError::new(
                    "deassert retired mediated SPI",
                    alloc::format!("guest SPI {}: {error}", self.spi.raw()),
                )
            })?;
        }
        self.unmask_host_irq().map_err(|error| {
            GicV3BackendError::new(
                "unmask retired mediated SPI",
                alloc::format!("guest SPI {}: {error:?}", self.spi.raw()),
            )
        })
    }

    fn unmask_host_irq(&self) -> Result<(), host_irq::IrqError> {
        if self
            .host_masked
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        let Some(registration) = *self.registration.lock() else {
            self.host_masked.store(true, Ordering::Release);
            return Err(host_irq::IrqError::NotFound);
        };
        if let Err(error) = host_irq::enable_irq(registration) {
            self.host_masked.store(true, Ordering::Release);
            return Err(error);
        }
        Ok(())
    }

    fn release_registration(&self) -> Result<(), host_irq::IrqError> {
        let Some(registration) = self.registration.lock().take() else {
            return Ok(());
        };
        host_irq::free_irq(registration)
    }
}

fn validate_spi(intid: u32, operation: &'static str) -> AxVmResult<SpiId> {
    SpiId::new(intid).map_err(|error| AxVmError::interrupt(operation, error))
}

fn request_host_irq(
    irq: host_irq::IrqId,
    intid: u32,
    delivery: &'static str,
    request: host_irq::IrqRequest,
) -> AxVmResult<IrqHandle> {
    host_irq::request_irq(irq, request).map_err(|error| {
        AxVmError::interrupt(
            "register GICv3 host SPI forwarding",
            alloc::format!("{delivery} host IRQ {irq:?}, guest INTID {intid}: {error:?}"),
        )
    })
}
