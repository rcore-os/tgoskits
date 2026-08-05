use alloc::{boxed::Box, format, string::String, sync::Arc, vec, vec::Vec};
use core::{any::Any, num::NonZeroU32, sync::atomic::AtomicU32, time::Duration};

use dma_api::{DeviceDma, DmaConstraints};
use log::{info, warn};
use mmio_api::Mmio;
use rdif_block::{
    BHardwareQueue, BlkError, BlockController, BlockControllerGroup, BlockGroupMember,
    ControllerEvent, ControllerState, ControllerUpdate, DeviceInfo, GroupControllerEvent,
    GroupControllerUpdate, SharedIrqEndpoint,
};

use crate::{
    command::LOGICAL_BLOCK_SIZE,
    queue::{AhciQueue, AhciQueueConfig, GeometryState},
    registers::{
        AhciHostIrq, CONTROL_EVENT_IRQ, HbaRegisters, IRQ_SOURCE_ID, PortIrqRoute, PortRegisters,
    },
};

const REGISTER_RETRY_DELAY: Duration = Duration::from_millis(1);
const MAX_TRANSFER_BYTES: usize = u16::MAX as usize * LOGICAL_BLOCK_SIZE;

/// AHCI initialization and runtime error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AhciError {
    /// The mapped register aperture cannot cover the requested register block.
    #[error("AHCI MMIO aperture is too small: actual={actual:#x}, required={required:#x}")]
    MmioTooSmall { actual: usize, required: usize },
    /// Firmware or HBA capability data names an inaccessible port.
    #[error("AHCI port {0} is outside the mapped aperture")]
    InvalidPort(u8),
    /// The HBA reports no implemented SATA port.
    #[error("AHCI HBA reports no usable SATA port")]
    NoUsablePort,
}

impl From<AhciError> for BlkError {
    fn from(error: AhciError) -> Self {
        match error {
            AhciError::MmioTooSmall { .. } | AhciError::InvalidPort(_) => BlkError::InvalidRequest,
            AhciError::NoUsablePort => BlkError::NotSupported,
        }
    }
}

/// Policy for firmware-provided implemented-port state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortMapPolicy {
    /// Trust the HBA ports-implemented register.
    Reported,
    /// Repair an empty register with the supplied nonzero bitmap.
    FallbackIfEmpty(NonZeroU32),
}

/// Policy for staggered spin-up capability and port power-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpinUpPolicy {
    /// Trust the HBA capability and power up only when it is reported.
    HardwareReported,
    /// Repair the capability when absent and request port spin-up.
    ForceAndRepairCapability,
}

/// Policy controlling Native Command Queuing negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NcqPolicy {
    /// Enable NCQ when both HBA and disk advertise it.
    Auto,
    /// Keep the port on the nonqueued depth-one path.
    Disabled,
}

/// Portable AHCI host configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AhciConfig {
    /// Firmware implemented-port handling.
    pub port_map: PortMapPolicy,
    /// Staggered spin-up handling.
    pub spin_up: SpinUpPolicy,
    /// NCQ negotiation policy.
    pub ncq: NcqPolicy,
}

impl AhciConfig {
    /// Generic PCI/FDT profile that trusts hardware capabilities.
    pub const fn generic() -> Self {
        Self {
            port_map: PortMapPolicy::Reported,
            spin_up: SpinUpPolicy::HardwareReported,
            ncq: NcqPolicy::Auto,
        }
    }

    /// LS2K profile for firmware that leaves PI and CAP.SSS incomplete.
    pub const fn ls2k() -> Self {
        Self {
            port_map: PortMapPolicy::FallbackIfEmpty(NonZeroU32::MIN),
            spin_up: SpinUpPolicy::ForceAndRepairCapability,
            ncq: NcqPolicy::Auto,
        }
    }
}

impl Default for AhciConfig {
    fn default() -> Self {
        Self::generic()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostLifecycle {
    New,
    Resetting,
    Ready,
    Stopped,
}

/// Portable owner of one AHCI HBA and all member SATA ports.
pub struct AhciHost {
    name: String,
    hba: HbaRegisters,
    dma: DeviceDma,
    config: AhciConfig,
    state: HostLifecycle,
}

impl AhciHost {
    /// Creates an AHCI host from mapped MMIO and a device-scoped DMA capability.
    ///
    /// # Errors
    ///
    /// Returns [`AhciError::MmioTooSmall`] when the fixed HBA header is not
    /// mapped. Port apertures are validated when the group is started.
    pub fn new(
        name: String,
        mmio: Mmio,
        dma: DeviceDma,
        config: AhciConfig,
    ) -> Result<Self, AhciError> {
        let hba = HbaRegisters::new(mmio)?;
        let dma = narrow_dma_capability(&dma, hba.dma_mask());
        info!(
            "{}: AHCI cap={:#010x} ports={} implemented={:#010x} slots={} dma_mask={:#018x}",
            name,
            hba.capabilities(),
            hba.max_ports(),
            hba.implemented_ports(),
            hba.command_slots(),
            dma.dma_mask()
        );
        Ok(Self {
            name,
            hba,
            dma,
            config,
            state: HostLifecycle::New,
        })
    }

    fn start(&mut self) -> Result<GroupControllerUpdate, BlkError> {
        if self.state != HostLifecycle::New {
            return Err(BlkError::InvalidRequest);
        }
        self.hba.set_interrupts_enabled(false);
        self.hba.begin_reset();
        self.state = HostLifecycle::Resetting;
        Ok(Self::register_pending())
    }

    fn finish_reset(&mut self) -> Result<GroupControllerUpdate, BlkError> {
        if !self.hba.reset_complete() {
            return Ok(Self::register_pending());
        }
        self.hba.enable_ahci();
        self.apply_platform_profile();
        let (members, routes) = self.create_port_members()?;
        let endpoint = SharedIrqEndpoint::new(
            IRQ_SOURCE_ID,
            Box::new(AhciHostIrq::new(self.hba.clone(), routes)),
        );
        self.state = HostLifecycle::Ready;
        Ok(GroupControllerUpdate::with_resources(
            ControllerState::Ready,
            members,
            vec![endpoint],
        ))
    }

    fn apply_platform_profile(&self) {
        if matches!(self.config.spin_up, SpinUpPolicy::ForceAndRepairCapability)
            && !self.hba.supports_staggered_spin_up()
        {
            let capabilities = self.hba.initialize_staggered_spin_up_capability();
            warn!(
                "{}: repaired missing CAP.SSS, readback={capabilities:#010x}",
                self.name
            );
        }
        if let PortMapPolicy::FallbackIfEmpty(fallback) = self.config.port_map
            && self.hba.implemented_ports() == 0
        {
            let repaired = self.hba.initialize_port_map(fallback.get());
            warn!(
                "{}: repaired empty PI with {:#010x}, readback={repaired:#010x}",
                self.name,
                fallback.get()
            );
        }
    }

    fn create_port_members(&self) -> Result<(Vec<BlockGroupMember>, Vec<PortIrqRoute>), BlkError> {
        let implemented = self.hba.implemented_ports();
        let mut members = Vec::new();
        let mut routes = Vec::new();
        for index in 0..self.hba.max_ports().min(32) {
            if implemented & (1_u32 << index) == 0 {
                continue;
            }
            let port = self.hba.port(index).map_err(BlkError::from)?;
            port.set_interrupts_enabled(false);
            port.clear_stale_status();
            if !matches!(self.config.spin_up, SpinUpPolicy::ForceAndRepairCapability)
                && !port.link_present()
            {
                info!("{}: skip empty AHCI port {}", self.name, index);
                continue;
            }
            let status_latch = Arc::new(AtomicU32::new(0));
            let member_name = format!("{}-p{}", self.name, index);
            let controller = AhciPortController::new(
                member_name,
                port.clone(),
                self.dma.clone(),
                Arc::clone(&status_latch),
                PortCapabilities {
                    command_slots: self.hba.command_slots(),
                    ncq: self.hba.supports_ncq(),
                    spin_up: self.hba.supports_staggered_spin_up()
                        || matches!(self.config.spin_up, SpinUpPolicy::ForceAndRepairCapability),
                },
                self.config.ncq,
            );
            routes.push(PortIrqRoute::new(usize::from(index), port, status_latch));
            members.push(BlockGroupMember::new(
                usize::from(index),
                Box::new(controller),
            ));
        }
        if members.is_empty() {
            return Err(AhciError::NoUsablePort.into());
        }
        Ok((members, routes))
    }

    fn rearm(&mut self, source_id: usize) -> Result<GroupControllerUpdate, BlkError> {
        if source_id != IRQ_SOURCE_ID || self.state != HostLifecycle::Ready {
            return Err(BlkError::InvalidRequest);
        }
        self.hba.set_interrupts_enabled(true);
        Ok(GroupControllerUpdate::state(ControllerState::Ready))
    }

    fn quiesce(&mut self) -> GroupControllerUpdate {
        self.hba.set_interrupts_enabled(false);
        GroupControllerUpdate::state(match self.state {
            HostLifecycle::Stopped => ControllerState::Shutdown,
            HostLifecycle::Ready => ControllerState::Ready,
            HostLifecycle::New | HostLifecycle::Resetting => ControllerState::RegisterPending {
                retry_after: REGISTER_RETRY_DELAY,
            },
        })
    }

    fn shutdown(&mut self) -> GroupControllerUpdate {
        self.hba.set_interrupts_enabled(false);
        self.state = HostLifecycle::Stopped;
        GroupControllerUpdate::state(ControllerState::Shutdown)
    }

    const fn register_pending() -> GroupControllerUpdate {
        GroupControllerUpdate::state(ControllerState::RegisterPending {
            retry_after: REGISTER_RETRY_DELAY,
        })
    }
}

impl rdif_block::DriverGeneric for AhciHost {
    fn name(&self) -> &str {
        &self.name
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl BlockControllerGroup for AhciHost {
    fn advance(&mut self, event: GroupControllerEvent) -> Result<GroupControllerUpdate, BlkError> {
        match event {
            GroupControllerEvent::Start => self.start(),
            GroupControllerEvent::RegisterRetry if self.state == HostLifecycle::Resetting => {
                self.finish_reset()
            }
            GroupControllerEvent::Irq(_) if self.state == HostLifecycle::Ready => {
                Ok(GroupControllerUpdate::state(ControllerState::Ready))
            }
            GroupControllerEvent::Rearm { source_id } => self.rearm(source_id),
            GroupControllerEvent::QuiesceIrqs => Ok(self.quiesce()),
            GroupControllerEvent::Shutdown => Ok(self.shutdown()),
            _ => Err(BlkError::InvalidRequest),
        }
    }
}

#[derive(Clone, Copy)]
struct PortCapabilities {
    command_slots: usize,
    ncq: bool,
    spin_up: bool,
}

#[derive(Clone)]
enum PortLifecycle {
    New,
    CommandStopping,
    FisStopping,
    LinkWait,
    Identifying,
    Ready,
    ShutdownCommandStopping,
    ShutdownFisStopping,
    Stopped,
}

struct AhciPortController {
    name: String,
    port: PortRegisters,
    dma: DeviceDma,
    geometry: Arc<GeometryState>,
    irq_status: Arc<AtomicU32>,
    queue: Option<AhciQueue>,
    capabilities: PortCapabilities,
    ncq_policy: NcqPolicy,
    state: PortLifecycle,
}

impl AhciPortController {
    fn new(
        name: String,
        port: PortRegisters,
        dma: DeviceDma,
        irq_status: Arc<AtomicU32>,
        capabilities: PortCapabilities,
        ncq_policy: NcqPolicy,
    ) -> Self {
        Self {
            name,
            port,
            dma,
            geometry: Arc::new(GeometryState::new()),
            irq_status,
            queue: None,
            capabilities,
            ncq_policy,
            state: PortLifecycle::New,
        }
    }

    fn start(&mut self, target_queues: usize) -> Result<ControllerUpdate, BlkError> {
        if !matches!(self.state, PortLifecycle::New) || target_queues == 0 {
            return Err(BlkError::InvalidRequest);
        }
        let queue = AhciQueue::new(
            self.name.clone(),
            self.port.clone(),
            self.dma.clone(),
            Arc::clone(&self.geometry),
            Arc::clone(&self.irq_status),
            AhciQueueConfig {
                hba_slots: self.capabilities.command_slots,
                hba_ncq: self.capabilities.ncq,
                ncq_policy: self.ncq_policy,
            },
        )?;
        self.port.set_interrupts_enabled(false);
        self.port.stop_command_engine();
        self.queue = Some(queue);
        self.state = PortLifecycle::CommandStopping;
        Ok(Self::register_pending())
    }

    fn retry_transition(&mut self) -> Result<ControllerUpdate, BlkError> {
        match self.state {
            PortLifecycle::CommandStopping => self.finish_command_stop(),
            PortLifecycle::FisStopping => self.finish_fis_stop(),
            PortLifecycle::LinkWait => self.finish_link_start(),
            PortLifecycle::ShutdownCommandStopping => self.finish_shutdown_command_stop(),
            PortLifecycle::ShutdownFisStopping => self.finish_shutdown_fis_stop(),
            _ => Err(BlkError::InvalidRequest),
        }
    }

    fn finish_command_stop(&mut self) -> Result<ControllerUpdate, BlkError> {
        if !self.port.command_engine_stopped() {
            return Ok(Self::register_pending());
        }
        self.port.stop_fis_receive();
        self.state = PortLifecycle::FisStopping;
        Ok(Self::register_pending())
    }

    fn finish_fis_stop(&mut self) -> Result<ControllerUpdate, BlkError> {
        if !self.port.fis_receive_stopped() {
            return Ok(Self::register_pending());
        }
        let queue = self.queue.as_ref().ok_or(BlkError::Io)?;
        self.port
            .program_dma_bases(queue.command_list_dma(), queue.received_fis_dma());
        self.port.clear_stale_status();
        self.port.power_up(self.capabilities.spin_up);
        self.port.start_fis_receive();
        self.port.start_command_engine();
        self.state = PortLifecycle::LinkWait;
        Ok(Self::register_pending())
    }

    fn finish_link_start(&mut self) -> Result<ControllerUpdate, BlkError> {
        if !self.port.link_present() || !self.port.task_file_ready() {
            return Ok(Self::register_pending());
        }
        if !self.port.is_sata_or_unknown() {
            return Err(BlkError::NotSupported);
        }
        self.port.clear_stale_status();
        let mut queue = self.queue.take().ok_or(BlkError::Io)?;
        queue.begin_identify()?;
        self.state = PortLifecycle::Identifying;
        let queues: Vec<BHardwareQueue> = vec![Box::new(queue)];
        Ok(ControllerUpdate::with_resources(
            ControllerState::WaitingForIrq,
            queues,
            Vec::new(),
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
        if self.geometry.is_ready() && matches!(self.state, PortLifecycle::Identifying) {
            self.state = PortLifecycle::Ready;
            info!(
                "{}: ready with {} blocks and NCQ depth {}",
                self.name,
                self.geometry.blocks(),
                self.geometry.queue_depth()
            );
        }
        Ok(self.current_update())
    }

    fn rearm(&mut self, source_id: usize) -> Result<ControllerUpdate, BlkError> {
        if source_id != IRQ_SOURCE_ID || self.geometry.is_failed() {
            return Err(BlkError::InvalidRequest);
        }
        if !matches!(
            self.state,
            PortLifecycle::Identifying | PortLifecycle::Ready
        ) {
            return Err(BlkError::InvalidRequest);
        }
        self.port.set_interrupts_enabled(true);
        Ok(self.current_update())
    }

    fn quiesce(&mut self) -> ControllerUpdate {
        self.port.set_interrupts_enabled(false);
        self.current_update()
    }

    fn begin_shutdown(&mut self) -> Result<ControllerUpdate, BlkError> {
        if matches!(self.state, PortLifecycle::Stopped) {
            return Ok(ControllerUpdate::state(ControllerState::Shutdown));
        }
        self.port.set_interrupts_enabled(false);
        self.port.stop_command_engine();
        self.state = PortLifecycle::ShutdownCommandStopping;
        self.finish_shutdown_command_stop()
    }

    fn finish_shutdown_command_stop(&mut self) -> Result<ControllerUpdate, BlkError> {
        if !self.port.command_engine_stopped() {
            return Ok(Self::register_pending());
        }
        self.port.stop_fis_receive();
        self.state = PortLifecycle::ShutdownFisStopping;
        Ok(Self::register_pending())
    }

    fn finish_shutdown_fis_stop(&mut self) -> Result<ControllerUpdate, BlkError> {
        if !self.port.fis_receive_stopped() {
            return Ok(Self::register_pending());
        }
        self.state = PortLifecycle::Stopped;
        Ok(ControllerUpdate::state(ControllerState::Shutdown))
    }

    fn current_update(&self) -> ControllerUpdate {
        let state = match self.state {
            PortLifecycle::New
            | PortLifecycle::CommandStopping
            | PortLifecycle::FisStopping
            | PortLifecycle::LinkWait
            | PortLifecycle::ShutdownCommandStopping
            | PortLifecycle::ShutdownFisStopping => ControllerState::RegisterPending {
                retry_after: REGISTER_RETRY_DELAY,
            },
            PortLifecycle::Identifying => ControllerState::WaitingForIrq,
            PortLifecycle::Ready => ControllerState::Ready,
            PortLifecycle::Stopped => ControllerState::Shutdown,
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

impl rdif_block::DriverGeneric for AhciPortController {
    fn name(&self) -> &str {
        &self.name
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl BlockController for AhciPortController {
    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            name: Some("ahci"),
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
                if target_queues != 0 && matches!(self.state, PortLifecycle::Ready) =>
            {
                Ok(self.current_update())
            }
            ControllerEvent::Rearm { source_id } => self.rearm(source_id),
            ControllerEvent::QuiesceIrqs => Ok(self.quiesce()),
            ControllerEvent::Watchdog { .. } | ControllerEvent::Shutdown => self.begin_shutdown(),
            _ => Err(BlkError::InvalidRequest),
        }
    }
}

fn narrow_dma_capability(dma: &DeviceDma, hba_mask: u64) -> DeviceDma {
    let current = dma.constraints();
    let max_segment_size = current
        .max_segment_size
        .map_or(MAX_TRANSFER_BYTES, |limit| limit.min(MAX_TRANSFER_BYTES));
    dma.with_constraints(DmaConstraints {
        addr_mask: current.addr_mask.min(hba_mask),
        align: current.align.max(2),
        boundary: current.boundary,
        max_segment_size: Some(max_segment_size),
    })
}
