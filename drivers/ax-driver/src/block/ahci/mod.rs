//! AHCI queue and lifecycle integration.
//!
//! The controller task owns reset, link, IRQ rearm, and shutdown transitions;
//! one hctx owns the command slot and all DMA request state. The hard IRQ token
//! only masks and acknowledges fixed AHCI status registers.

extern crate alloc;

#[cfg(feature = "ls2k1000-ahci")]
use alloc::format;
use alloc::{boxed::Box, sync::Arc, vec};
use core::{any::Any, ptr::NonNull, time::Duration};

use log::{info, warn};
#[cfg(feature = "ahci")]
use pcie::CommandRegister;
use rdif_block::{
    BHardwareQueue, BlkError, BlockController, ControllerEvent, ControllerState, ControllerUpdate,
    DeviceInfo, IrqEndpoint,
};
use rdrive::probe::OnProbeError;
#[cfg(feature = "ahci")]
use rdrive::probe::pci::{FnOnProbe, ProbePci};
#[cfg(feature = "ls2k1000-ahci")]
use rdrive::register::ProbeFdt;

#[cfg(feature = "ls2k1000-ahci")]
use crate::block::PlatformDeviceBlock;
#[cfg(feature = "ahci")]
use crate::{PciIrqRequirement, block::ProbePciBlock};

mod command;
mod queue;
mod registers;

use command::LOGICAL_BLOCK_SIZE;
use queue::{AhciQueue, GeometryState};
use registers::{
    AhciIrqHandler, CONTROL_EVENT_IRQ, HbaRegisters, IRQ_SOURCE_ID, PortRegisters, QUEUE_ID,
};

#[cfg(feature = "ahci")]
const DEVICE_NAME: &str = "ahci";
#[cfg(feature = "ls2k1000-ahci")]
const LS2K1000_DEVICE_NAME: &str = "ls2k1000-ahci";
#[cfg(feature = "ls2k1000-ahci")]
const LS2K1000_DEFAULT_MMIO_SIZE: usize = 0x10_000;
const REGISTER_RETRY_DELAY: Duration = Duration::from_millis(1);

#[derive(Clone, Copy, Default)]
struct AhciPlatformConfig {
    port_map_fallback: u32,
    repair_staggered_spin_up_capability: bool,
    force_spin_up: bool,
}

#[cfg(feature = "ls2k1000-ahci")]
// JL-LSGD2K10 firmware leaves PI clear and the integrated controller ignores
// PxCMD.SUD unless CAP.SSS is exposed. Keep both corrections confined to this
// FDT profile; PCI AHCI continues to trust and preserve the reported registers.
const LS2K1000_CONFIG: AhciPlatformConfig = AhciPlatformConfig {
    port_map_fallback: 1,
    repair_staggered_spin_up_capability: true,
    force_spin_up: true,
};

#[cfg(feature = "ahci")]
crate::model_register!(
    name: "AHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci as FnOnProbe,
    }],
);

#[cfg(feature = "ls2k1000-ahci")]
crate::model_register!(
    name: "LS2K1000 AHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &[
            "loongson,ls-ahci",
            "loongson,ls2k1000-ahci",
            "loongson,2k1000-ahci",
            "generic-ahci",
            "snps,dwc-ahci",
        ],
        on_probe: probe_fdt,
    }],
);

#[derive(Clone, Copy)]
enum Lifecycle {
    New,
    HbaReset,
    PortCommandStopping(PortRegisters),
    PortFisStopping(PortRegisters),
    LinkWait(PortRegisters),
    Identifying(PortRegisters),
    Ready(PortRegisters),
    CommandStopping(PortRegisters),
    FisStopping(PortRegisters),
    Stopped,
}

struct AhciController {
    name: &'static str,
    hba: HbaRegisters,
    dma: dma_api::DeviceDma,
    geometry: Arc<GeometryState>,
    queue: Option<AhciQueue>,
    config: AhciPlatformConfig,
    state: Lifecycle,
}

impl AhciController {
    /// # Safety
    ///
    /// `mmio_base` must be an exclusively managed AHCI register mapping that
    /// outlives the controller, queue, and registered IRQ token.
    unsafe fn new(name: &'static str, mmio_base: NonNull<u8>, config: AhciPlatformConfig) -> Self {
        // SAFETY: Forwarded from this constructor's contract.
        let hba = unsafe { HbaRegisters::new(mmio_base) };
        let dma = axklib::dma::device_with_mask(hba.dma_mask());
        info!(
            "{name}: AHCI HBA cap={:#010x} ports={} implemented={:#010x} dma_mask={:#018x}",
            hba.capabilities(),
            hba.max_ports(),
            hba.implemented_ports(),
            hba.dma_mask()
        );
        Self {
            name,
            hba,
            dma,
            geometry: Arc::new(GeometryState::new()),
            queue: None,
            config,
            state: Lifecycle::New,
        }
    }

    fn start(&mut self, target_queues: usize) -> Result<ControllerUpdate, BlkError> {
        if !matches!(self.state, Lifecycle::New) || target_queues == 0 {
            return Err(BlkError::InvalidRequest);
        }
        self.hba.begin_reset();
        self.state = Lifecycle::HbaReset;
        Ok(Self::register_pending())
    }

    fn retry_transition(&mut self) -> Result<ControllerUpdate, BlkError> {
        match self.state {
            Lifecycle::HbaReset => self.finish_hba_reset(),
            Lifecycle::PortCommandStopping(port) => self.finish_port_command_stop(port),
            Lifecycle::PortFisStopping(port) => self.finish_port_fis_stop(port),
            Lifecycle::LinkWait(port) => self.finish_link_start(port),
            Lifecycle::CommandStopping(port) => self.finish_shutdown_command_stop(port),
            Lifecycle::FisStopping(port) => self.finish_shutdown_fis_stop(port),
            _ => Err(BlkError::InvalidRequest),
        }
    }

    fn finish_hba_reset(&mut self) -> Result<ControllerUpdate, BlkError> {
        if !self.hba.reset_complete() {
            return Ok(Self::register_pending());
        }
        self.hba.enable_ahci();
        if self.config.repair_staggered_spin_up_capability && !self.hba.supports_staggered_spin_up()
        {
            let capabilities = self.hba.initialize_staggered_spin_up_capability();
            warn!(
                "{}: repaired missing AHCI staggered-spin-up capability, CAP readback \
                 {capabilities:#010x}",
                self.name
            );
        }
        if self.hba.implemented_ports() == 0 && self.config.port_map_fallback != 0 {
            let repaired = self.hba.initialize_port_map(self.config.port_map_fallback);
            warn!(
                "{}: firmware left AHCI PI empty; applied platform port map {:#010x}, readback \
                 {repaired:#010x}",
                self.name, self.config.port_map_fallback
            );
        }
        let port = self.select_port().ok_or(BlkError::NotSupported)?;
        let queue = AhciQueue::new(
            self.name,
            port,
            self.dma.clone(),
            Arc::clone(&self.geometry),
        )?;
        port.set_interrupts_enabled(false);
        port.stop_command_engine();
        self.queue = Some(queue);
        self.state = Lifecycle::PortCommandStopping(port);
        info!(
            "{}: selected AHCI port {} cmd={:#010x} ssts={:#010x} tfd={:#010x}",
            self.name,
            port.index(),
            port.command_state(),
            port.sata_status(),
            port.task_file_status()
        );
        Ok(Self::register_pending())
    }

    fn finish_port_command_stop(
        &mut self,
        port: PortRegisters,
    ) -> Result<ControllerUpdate, BlkError> {
        if !port.command_engine_stopped() {
            return Ok(Self::register_pending());
        }
        info!(
            "{}: AHCI port {} command engine stopped cmd={:#010x}",
            self.name,
            port.index(),
            port.command_state()
        );
        port.stop_fis_receive();
        self.state = Lifecycle::PortFisStopping(port);
        Ok(Self::register_pending())
    }

    fn finish_port_fis_stop(&mut self, port: PortRegisters) -> Result<ControllerUpdate, BlkError> {
        if !port.fis_receive_stopped() {
            return Ok(Self::register_pending());
        }
        info!(
            "{}: AHCI port {} FIS receive stopped cmd={:#010x}",
            self.name,
            port.index(),
            port.command_state()
        );
        let queue = self.queue.as_ref().ok_or(BlkError::Io)?;
        port.program_dma_bases(queue.command_list_dma(), queue.received_fis_dma());
        port.clear_stale_status();
        port.power_up(self.hba.supports_staggered_spin_up() || self.config.force_spin_up);
        port.start_fis_receive();
        port.start_command_engine();
        info!(
            "{}: AHCI port {} engines started cmd={:#010x} ssts={:#010x} tfd={:#010x}",
            self.name,
            port.index(),
            port.command_state(),
            port.sata_status(),
            port.task_file_status()
        );
        self.state = Lifecycle::LinkWait(port);
        Ok(Self::register_pending())
    }

    fn finish_link_start(&mut self, port: PortRegisters) -> Result<ControllerUpdate, BlkError> {
        if !port.link_present() || !port.task_file_ready() {
            return Ok(Self::register_pending());
        }
        info!(
            "{}: AHCI port {} link ready ssts={:#010x} tfd={:#010x}",
            self.name,
            port.index(),
            port.sata_status(),
            port.task_file_status()
        );
        port.clear_stale_status();
        let mut queue = self.queue.take().ok_or(BlkError::Io)?;
        let irq_status = queue.irq_status_latch();
        queue.begin_identify()?;
        let endpoint = IrqEndpoint::new(
            IRQ_SOURCE_ID,
            1_u64 << QUEUE_ID,
            Box::new(AhciIrqHandler::new(port, irq_status)),
        );
        self.state = Lifecycle::Identifying(port);
        let queues: alloc::vec::Vec<BHardwareQueue> = vec![Box::new(queue)];
        Ok(ControllerUpdate::with_resources(
            ControllerState::WaitingForIrq,
            queues,
            vec![endpoint],
        ))
    }

    fn observe_irq(
        &mut self,
        event: rdif_block::ControlEvent,
    ) -> Result<ControllerUpdate, BlkError> {
        if event.source_id() != IRQ_SOURCE_ID || event.bits() & CONTROL_EVENT_IRQ == 0 {
            return Ok(self.current_update());
        }
        if self.geometry.is_failed() {
            return Err(BlkError::Io);
        }
        if self.geometry.is_ready()
            && let Lifecycle::Identifying(port) = self.state
        {
            self.state = Lifecycle::Ready(port);
            info!(
                "{}: AHCI port {} ready with {} logical blocks",
                self.name,
                port.index(),
                self.geometry.blocks()
            );
        }
        Ok(self.current_update())
    }

    fn rearm(&mut self, source_id: usize) -> Result<ControllerUpdate, BlkError> {
        if source_id != IRQ_SOURCE_ID || self.geometry.is_failed() {
            return Err(BlkError::InvalidRequest);
        }
        let port = self.active_port().ok_or(BlkError::InvalidRequest)?;
        match self.state {
            Lifecycle::Identifying(_) | Lifecycle::Ready(_) => {
                port.set_interrupts_enabled(true);
                self.hba.set_interrupts_enabled(true);
                Ok(self.current_update())
            }
            _ => Err(BlkError::InvalidRequest),
        }
    }

    fn quiesce_irqs(&mut self) -> ControllerUpdate {
        if let Some(port) = self.active_port() {
            port.set_interrupts_enabled(false);
        }
        self.hba.set_interrupts_enabled(false);
        self.current_update()
    }

    fn begin_shutdown(&mut self) -> Result<ControllerUpdate, BlkError> {
        if matches!(self.state, Lifecycle::Stopped) {
            return Ok(ControllerUpdate::state(ControllerState::Shutdown));
        }
        self.quiesce_irqs();
        let Some(port) = self.active_port() else {
            self.state = Lifecycle::Stopped;
            return Ok(ControllerUpdate::state(ControllerState::Shutdown));
        };
        port.stop_command_engine();
        self.state = Lifecycle::CommandStopping(port);
        self.finish_shutdown_command_stop(port)
    }

    fn finish_shutdown_command_stop(
        &mut self,
        port: PortRegisters,
    ) -> Result<ControllerUpdate, BlkError> {
        if !port.command_engine_stopped() {
            return Ok(Self::register_pending());
        }
        port.stop_fis_receive();
        self.state = Lifecycle::FisStopping(port);
        Ok(Self::register_pending())
    }

    fn finish_shutdown_fis_stop(
        &mut self,
        port: PortRegisters,
    ) -> Result<ControllerUpdate, BlkError> {
        if !port.fis_receive_stopped() {
            return Ok(Self::register_pending());
        }
        self.hba.set_interrupts_enabled(false);
        self.state = Lifecycle::Stopped;
        Ok(ControllerUpdate::state(ControllerState::Shutdown))
    }

    fn select_port(&self) -> Option<PortRegisters> {
        let implemented = self.hba.implemented_ports();
        let max_ports = self.hba.max_ports().min(32);
        let mut first = None;
        for index in 0..max_ports {
            if implemented & (1_u32 << index) == 0 {
                continue;
            }
            let port = self.hba.port(index);
            first.get_or_insert(port);
            if port.link_present() {
                return Some(port);
            }
        }
        first
    }

    fn active_port(&self) -> Option<PortRegisters> {
        match self.state {
            Lifecycle::PortCommandStopping(port)
            | Lifecycle::PortFisStopping(port)
            | Lifecycle::LinkWait(port)
            | Lifecycle::Identifying(port)
            | Lifecycle::Ready(port)
            | Lifecycle::CommandStopping(port)
            | Lifecycle::FisStopping(port) => Some(port),
            Lifecycle::New | Lifecycle::HbaReset | Lifecycle::Stopped => None,
        }
    }

    fn current_update(&self) -> ControllerUpdate {
        let state = match self.state {
            Lifecycle::New => ControllerState::RegisterPending {
                retry_after: REGISTER_RETRY_DELAY,
            },
            Lifecycle::HbaReset
            | Lifecycle::PortCommandStopping(_)
            | Lifecycle::PortFisStopping(_)
            | Lifecycle::LinkWait(_) => ControllerState::RegisterPending {
                retry_after: REGISTER_RETRY_DELAY,
            },
            Lifecycle::Identifying(_) => ControllerState::WaitingForIrq,
            Lifecycle::Ready(_) => ControllerState::Ready,
            Lifecycle::CommandStopping(_) | Lifecycle::FisStopping(_) => {
                ControllerState::RegisterPending {
                    retry_after: REGISTER_RETRY_DELAY,
                }
            }
            Lifecycle::Stopped => ControllerState::Shutdown,
        };
        let update = ControllerUpdate::state(state);
        if state == ControllerState::Ready {
            update.with_device_info(self.device_info())
        } else {
            update
        }
    }

    const fn register_pending() -> ControllerUpdate {
        ControllerUpdate::state(ControllerState::RegisterPending {
            retry_after: REGISTER_RETRY_DELAY,
        })
    }
}

impl rdrive::DriverGeneric for AhciController {
    fn name(&self) -> &str {
        self.name
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl BlockController for AhciController {
    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            name: Some(self.name),
            model: Some("sata-ahci"),
            ..DeviceInfo::new(self.geometry.blocks(), LOGICAL_BLOCK_SIZE)
        }
    }

    fn max_io_queues(&self) -> usize {
        1
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { target_queues } => self.start(target_queues),
            ControllerEvent::RegisterRetry => self.retry_transition(),
            ControllerEvent::Irq(event) => self.observe_irq(event),
            ControllerEvent::OnlineSmp { target_queues }
                if target_queues != 0 && matches!(self.state, Lifecycle::Ready(_)) =>
            {
                Ok(self.current_update())
            }
            ControllerEvent::Rearm { source_id } => self.rearm(source_id),
            ControllerEvent::QuiesceIrqs => Ok(self.quiesce_irqs()),
            ControllerEvent::Watchdog { .. } | ControllerEvent::Shutdown => self.begin_shutdown(),
            _ => Err(BlkError::InvalidRequest),
        }
    }
}

#[cfg(feature = "ahci")]
fn probe_pci(mut probe: ProbePci<'_>) -> Result<(), OnProbeError> {
    let class = probe.endpoint().revision_and_class();
    if (class.base_class, class.sub_class) != (0x01, 0x06) {
        return Err(OnProbeError::NotMatch);
    }
    let bar = probe
        .endpoint()
        .bar_mmio(5)
        .or_else(|| probe.endpoint().bar_mmio(0))
        .ok_or_else(|| OnProbeError::other("AHCI MMIO BAR missing"))?;
    probe.endpoint_mut().update_command(|mut command| {
        command.insert(CommandRegister::MEMORY_ENABLE | CommandRegister::BUS_MASTER_ENABLE);
        command.remove(CommandRegister::INTERRUPT_DISABLE);
        command
    });
    let mmio = crate::mmio::iomap(bar.start, bar.count().max(1))?;
    // SAFETY: `iomap` supplies the exclusively registered PCI BAR mapping and
    // the rdrive device owns it through teardown.
    let controller =
        unsafe { AhciController::new(DEVICE_NAME, mmio, AhciPlatformConfig::default()) };
    probe.register_block(controller, PciIrqRequirement::Required)?;
    Ok(())
}

#[cfg(feature = "ls2k1000-ahci")]
fn probe_fdt(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let binding = crate::binding_info_from_fdt(probe.info())?;
    if binding.irq().is_none() {
        return Err(OnProbeError::other(
            "LS2K1000 AHCI requires an interrupt; polling fallback is unsupported",
        ));
    }
    let (info, platform) = probe.into_parts();
    let register = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;
    let size = register.size.unwrap_or(LS2K1000_DEFAULT_MMIO_SIZE as u64) as usize;
    let mmio = crate::mmio::iomap(register.address as usize, size)?;
    // SAFETY: `iomap` supplies the FDT-described controller mapping and rdrive
    // serializes probe ownership for this node.
    let controller = unsafe { AhciController::new(LS2K1000_DEVICE_NAME, mmio, LS2K1000_CONFIG) };
    platform.register_block_with_info(controller, binding);
    Ok(())
}
