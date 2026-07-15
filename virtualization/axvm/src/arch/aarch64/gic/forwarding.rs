//! Host SPI forwarding into an emulated VM-local Distributor.

use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use arm_vgic::SpiId;
use ax_kspin::SpinRaw;
use ax_std::os::arceos::modules::ax_hal::irq::{self as host_irq, IrqHandle, IrqReturn};
use axdevice::{
    ControllerInputId, InterruptControllerId, InterruptTopology, InterruptTriggerMode, IrqLine,
    WiredIrqRequest,
};

use super::{AxvmGicV3Backend, passthrough::resolve_host_irq};
use crate::{AxVmError, AxVmResult};

/// VM-owned registrations that forward physical SPIs through topology lines.
pub(crate) struct HostSpiForwarding {
    backend: Arc<AxvmGicV3Backend>,
    spis: Vec<Arc<ForwardedSpi>>,
}

impl HostSpiForwarding {
    /// Connects every discovered SPI and atomically owns the resulting host actions.
    pub(crate) fn connect(
        topology: &InterruptTopology,
        controller: InterruptControllerId,
        spis: &[u32],
        backend: Arc<AxvmGicV3Backend>,
    ) -> AxVmResult<Self> {
        let mut forwarding = Self {
            backend,
            spis: Vec::with_capacity(spis.len()),
        };
        for spi in spis {
            forwarding.connect_spi(topology, controller, *spi)?;
        }
        Ok(forwarding)
    }

    fn connect_spi(
        &mut self,
        topology: &InterruptTopology,
        controller: InterruptControllerId,
        spi: u32,
    ) -> AxVmResult {
        let intid = spi.checked_add(32).ok_or_else(|| {
            AxVmError::invalid_config("emulated GICv3 host SPI INTID overflows u32")
        })?;
        let spi = SpiId::new(intid)
            .map_err(|error| AxVmError::interrupt("validate emulated GICv3 SPI", error))?;
        let input = usize::try_from(intid).map_err(|_| {
            AxVmError::invalid_config("emulated GICv3 SPI INTID does not fit usize")
        })?;
        let line = topology.connect_irq(WiredIrqRequest::for_controller(
            controller,
            ControllerInputId::new(input),
            InterruptTriggerMode::EdgeTriggered,
        ))?;
        let irq = resolve_host_irq(intid)
            .map_err(|error| AxVmError::interrupt("resolve emulated GICv3 host SPI", error))?;
        let forwarding = Arc::new(ForwardedSpi::new(spi, irq, line));
        let handler = forwarding.clone();
        let request = host_irq::IrqRequest::new(move |_| handler.handle_host_irq())
            .share_mode(host_irq::ShareMode::Shared)
            .auto_enable(host_irq::AutoEnable::No);
        let registration = host_irq::request_irq(irq, request).map_err(|error| {
            AxVmError::interrupt(
                "register emulated GICv3 host SPI forwarding",
                alloc::format!("host IRQ {irq:?}, guest INTID {intid}: {error:?}"),
            )
        })?;
        forwarding.install_registration(registration)?;
        self.spis.push(forwarding.clone());
        self.backend
            .register_emulated_spi(spi, Arc::downgrade(&forwarding))
            .map_err(|error| AxVmError::interrupt("register emulated GICv3 retirement", error))?;
        host_irq::enable_irq(registration).map_err(|error| {
            AxVmError::interrupt(
                "enable emulated GICv3 host SPI forwarding",
                alloc::format!("host IRQ {irq:?}, guest INTID {intid}: {error:?}"),
            )
        })?;
        Ok(())
    }
}

impl Drop for HostSpiForwarding {
    fn drop(&mut self) {
        for spi in self.spis.drain(..).rev() {
            self.backend.unregister_emulated_spi(spi.spi());
            if let Err(error) = spi.release_registration() {
                warn!(
                    "failed to release emulated GICv3 host SPI forwarding for {:?}: {error:?}",
                    spi.host_irq()
                );
            }
        }
    }
}

pub(super) struct ForwardedSpi {
    spi: SpiId,
    host_irq: host_irq::IrqId,
    line: IrqLine,
    registration: SpinRaw<Option<IrqHandle>>,
    host_masked: AtomicBool,
}

impl ForwardedSpi {
    fn new(spi: SpiId, host_irq: host_irq::IrqId, line: IrqLine) -> Self {
        Self {
            spi,
            host_irq,
            line,
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
                "host IRQ {:?} already has an emulated GICv3 forwarding action",
                self.host_irq
            )));
        }
        *slot = Some(registration);
        Ok(())
    }

    fn handle_host_irq(&self) -> IrqReturn {
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
                "host IRQ {:?} arrived before its emulated GICv3 forwarding action was installed",
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
        match self.line.pulse() {
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

    pub(super) fn unmask_host_irq(&self) -> Result<(), host_irq::IrqError> {
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
