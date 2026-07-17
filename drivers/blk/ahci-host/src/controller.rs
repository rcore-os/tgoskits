use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{any::Any, mem};

use dma_api::{DeviceDma, DmaOp};
use mmio_api::{MmioAddr, MmioOp};
use rdif_block::{
    BlkError, ControllerEpoch, ControllerInitEndpoint, ControllerReady, DeviceInfo, DmaQuiesced,
    DriverGeneric, IdList, InitError, InitInput, InitIrqProgress, InitPoll, InitialController,
    InterruptLifecycle, IrqHandler, IrqSourceInfo, IrqSourceList, LifecycleEndpoint, QueueHandle,
    QueueLimits, RecoveryCause,
};

use crate::{
    AhciConfig, AhciError,
    initialization::{AhciInitialization, ControllerInitState},
    irq::HostShared,
    lifecycle::AhciLifecycle,
    queue::{AhciPortQueue, QueueBinding, ReadyPort},
    registers::{
        CAP_S64A, GHC_AE, GHC_HR, GHC_IE, HOST_CAP, HOST_GHC, HOST_PI, MAX_PORTS, MappedRegisters,
        PX_IE, SharedRegisters, write_port,
    },
};

/// Masked AHCI discovery object and shared HBA controller.
///
/// One host owns initialization, the destructive IRQ endpoint, and DMA
/// lifecycle for every implemented port. Each identified ATA disk must be
/// extracted as an independent [`AhciPortDevice`]; ports are never exposed as
/// interchangeable hardware queues of one logical block device.
///
/// ```compile_fail
/// fn require_single_device_interface<T: rdif_block::Interface>() {}
/// require_single_device_interface::<ahci_host::AhciHost>();
/// ```
pub struct AhciHost {
    name: &'static str,
    shared: Arc<HostShared>,
    dma: DeviceDma,
    config: AhciConfig,
    initialization: AhciInitialization,
    lifecycle: AhciLifecycle,
    ready_ports: Vec<Option<ReadyPort>>,
}

/// One independently addressed ATA disk attached to an AHCI host port.
///
/// The owning [`AhciHost`] must remain retained while this view or its queue is
/// active because recovery, IRQ routing, and exclusive handoff are HBA-wide.
pub struct AhciPortDevice {
    name: &'static str,
    port: usize,
    ata: crate::ata::AtaDevice,
    ready: Option<ReadyPort>,
    shared: Arc<HostShared>,
    binding: QueueBinding,
}

impl AhciHost {
    /// Maps an AHCI BAR, masks interrupt delivery, and constructs valid driver
    /// state without issuing a reset, ATA command, or queue doorbell.
    pub fn discover(
        name: &'static str,
        bar_addr: impl Into<MmioAddr>,
        bar_size: usize,
        dma_mask: u64,
        dma_op: &'static dyn DmaOp,
        mmio_op: &'static dyn MmioOp,
        config: AhciConfig,
    ) -> Result<Self, AhciError> {
        let config = config.validate()?;
        mmio_api::init(mmio_op);
        let mapping = mmio_api::ioremap(bar_addr.into(), bar_size)?;
        let registers: SharedRegisters = Arc::new(MappedRegisters::new(mapping)?);
        mask_discovered_interrupts(registers.as_ref());
        Ok(Self::from_parts(
            name,
            registers,
            DeviceDma::new_legacy(dma_mask, dma_op),
            config,
        ))
    }

    /// Returns the current discovery-to-ready phase without touching MMIO.
    pub fn controller_init_state(&self) -> ControllerInitState {
        self.initialization.state()
    }

    /// Returns the bounded discovery-to-ready controller state machine.
    pub fn controller_init(&mut self) -> ControllerInitEndpoint<'_> {
        if matches!(self.initialization.state(), ControllerInitState::Ready) {
            ControllerInitEndpoint::Ready
        } else {
            ControllerInitEndpoint::Pending(self)
        }
    }

    /// Returns the HBA-wide DMA shutdown, handoff, and recovery endpoint.
    pub fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        LifecycleEndpoint::Interrupt(self)
    }

    /// Returns identified ATA ports whose device views have not been taken.
    pub fn available_port_ids(&self) -> IdList {
        let mut ports = IdList::none();
        for ready in self.ready_ports.iter().filter_map(Option::as_ref) {
            ports.insert(ready.port);
        }
        ports
    }

    /// Extracts one logical block-device view for an identified ATA port.
    ///
    /// `device_name` names this disk, not the shared HBA. Callers should assign
    /// a unique shutdown-lifetime name to every extracted port.
    ///
    /// # Errors
    ///
    /// Returns [`AhciError::PortUnavailable`] before initialization completes,
    /// for a non-ATA port, or after that port view was already extracted.
    pub fn take_port_device(
        &mut self,
        port: usize,
        device_name: &'static str,
    ) -> Result<AhciPortDevice, AhciError> {
        if !matches!(self.initialization.state(), ControllerInitState::Ready) {
            return Err(AhciError::PortUnavailable { port });
        }
        let binding = QueueBinding {
            name: device_name,
            dma_mask: self.active_dma_mask(),
            dma_domain: self.dma.domain_id(),
            irq_source_id: self.config.irq_source_id,
            request_timeout_ns: self.config.command_timeout_ns,
            controller_cookie: self.controller_cookie(),
        };
        let ready = self
            .ready_ports
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|ready| ready.port == port))
            .and_then(Option::take)
            .ok_or(AhciError::PortUnavailable { port })?;
        let ata = ready.ata;
        Ok(AhciPortDevice {
            name: device_name,
            port,
            ata,
            ready: Some(ready),
            shared: Arc::clone(&self.shared),
            binding,
        })
    }

    /// Enables the already-bound shared IRQ action and all ready port sources.
    ///
    /// # Errors
    ///
    /// Returns [`BlkError::Other`] if the move-only IRQ endpoint for the
    /// current initialization or normal-I/O phase is not alive.
    pub fn enable_irq(&self) -> Result<(), BlkError> {
        let handler_live = if matches!(self.initialization.state(), ControllerInitState::Ready) {
            self.shared.io_handler_live()
        } else {
            self.shared.initial_handler_live()
        };
        if !handler_live {
            return Err(BlkError::Other("AHCI IRQ handler is not live"));
        }
        // Publish the software acknowledgement owner before unmasking any
        // device source. The OS action is already installed at this boundary.
        self.shared.set_irq_delivery_enabled(true);
        let ghc = self.shared.registers().read32(HOST_GHC);
        if self.shared.ready_ports() == 0 {
            // Initial activation has not read PI or reset firmware state yet.
            // Keep the controller-global source masked until the bounded FSM
            // completes HBA reset; otherwise inherited PxIE bits could assert
            // an interrupt whose implemented-port ownership is still unknown.
            self.shared.registers().write32(HOST_GHC, ghc & !GHC_IE);
            return Ok(());
        }
        self.shared.unmask_ready_ports();
        self.shared
            .registers()
            .write32(HOST_GHC, ghc | GHC_AE | GHC_IE);
        Ok(())
    }

    /// Masks every port and the controller-global IRQ source.
    pub fn disable_irq(&self) -> Result<(), BlkError> {
        // Keep the stable snapshot endpoint active until every device source
        // is masked. An interrupt racing this sequence can still be owned and
        // acknowledged instead of becoming an unhandled level assertion.
        self.shared.mask_all_ports();
        let ghc = self.shared.registers().read32(HOST_GHC);
        self.shared.registers().write32(HOST_GHC, ghc & !GHC_IE);
        self.shared.set_irq_delivery_enabled(false);
        Ok(())
    }

    /// Reports whether a shared IRQ endpoint owns destructive status reads.
    pub fn is_irq_enabled(&self) -> bool {
        self.shared.irq_delivery_enabled()
    }

    /// Describes the shared logical IRQ source and its global port queue IDs.
    pub fn irq_sources(&self) -> IrqSourceList {
        vec![IrqSourceInfo::new(
            self.config.irq_source_id,
            IdList::from_bits(u64::from(self.shared.ready_ports())),
        )]
    }

    /// Moves the normal-I/O destructive IRQ endpoint to its runtime owner.
    pub fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn IrqHandler>> {
        if source_id != self.config.irq_source_id
            || !matches!(self.initialization.state(), ControllerInitState::Ready)
        {
            return None;
        }
        self.shared.take_io_handler()
    }

    fn from_parts(
        name: &'static str,
        registers: SharedRegisters,
        dma: DeviceDma,
        config: AhciConfig,
    ) -> Self {
        Self {
            name,
            shared: HostShared::new(registers),
            dma,
            config,
            initialization: AhciInitialization::discovered(),
            lifecycle: AhciLifecycle::running(),
            ready_ports: Vec::new(),
        }
    }

    fn controller_cookie(&self) -> usize {
        Arc::as_ptr(&self.shared).expose_provenance()
    }

    fn active_dma_mask(&self) -> u64 {
        if self.shared.registers().read32(HOST_CAP) & CAP_S64A != 0 {
            self.dma.dma_mask()
        } else {
            self.dma.dma_mask().min(u64::from(u32::MAX))
        }
    }

    #[cfg(test)]
    pub(crate) fn from_test_parts(
        name: &'static str,
        registers: SharedRegisters,
        dma: DeviceDma,
        config: AhciConfig,
    ) -> Self {
        Self::from_parts(name, registers, dma, config)
    }
}

fn mask_discovered_interrupts(registers: &dyn crate::registers::RegisterIo) {
    let ghc = registers.read32(HOST_GHC);
    // HR is a write-one command, not ordinary retained state. Never copy an
    // in-progress firmware reset bit back while performing the mask-only
    // discovery transition.
    registers.write32(HOST_GHC, ghc & !(GHC_IE | GHC_HR));

    let implemented_ports = registers.read32(HOST_PI);
    for port in 0..MAX_PORTS {
        if implemented_ports & (1 << port) != 0 {
            write_port(registers, port, PX_IE, 0);
        }
    }
}

impl DriverGeneric for AhciHost {
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

impl AhciPortDevice {
    /// Returns the physical HBA port identity used in shared IRQ events.
    pub const fn port_id(&self) -> usize {
        self.port
    }

    /// Returns geometry for this ATA disk only.
    pub fn device_info(&self) -> DeviceInfo {
        self.ata.device_info(self.name)
    }

    /// Returns queue limits for this ATA disk only.
    pub fn queue_limits(&self) -> QueueLimits {
        self.binding.queue_info(self.port, self.ata).limits
    }

    /// Creates this disk's single serialized hardware queue.
    pub fn create_queue(&mut self) -> Option<QueueHandle> {
        let ready = self.ready.take()?;
        let queue = AhciPortQueue::new(ready, Arc::clone(&self.shared), self.binding);
        Some(QueueHandle::new(Box::new(queue)))
    }
}

impl DriverGeneric for AhciPortDevice {
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

impl Drop for AhciPortDevice {
    fn drop(&mut self) {
        let Some(ready) = self.ready.take() else {
            return;
        };
        self.shared.port(self.port).set_online(false);
        self.shared.mask_port(self.port);
        ready.quarantine();
        // A disk view may be abandoned while its FIS receive engine still
        // references the shared BAR and command memory. Retain the HBA anchor
        // fail-closed; normal teardown goes through controller lifecycle and
        // queue shutdown before dropping this view.
        mem::forget(Arc::clone(&self.shared));
    }
}

impl InitialController for AhciHost {
    fn irq_sources(&self) -> IdList {
        let mut sources = IdList::none();
        sources.insert(self.config.irq_source_id);
        sources
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn IrqHandler>> {
        if source_id != self.config.irq_source_id
            || !matches!(self.initialization.state(), ControllerInitState::Discovered)
        {
            return None;
        }
        self.shared.take_initial_handler()
    }

    fn service_deferred_irq(&mut self, _source_id: usize) -> InitIrqProgress {
        // AHCI owns ordinary W1C status entirely in the hard-IRQ endpoint and
        // therefore never requests task-side destructive acknowledgement.
        InitIrqProgress::Unhandled
    }

    fn poll_init(&mut self, input: InitInput) -> InitPoll<()> {
        self.initialization.poll(
            &self.shared,
            &self.dma,
            self.config,
            &mut self.ready_ports,
            input,
        )
    }
}

impl InterruptLifecycle for AhciHost {
    fn controller_cookie(&self) -> usize {
        self.controller_cookie()
    }

    fn service_deferred_irq(&mut self, _source_id: usize) -> InitIrqProgress {
        // The AHCI endpoint either owns and clears the port/host W1C latches
        // in hard IRQ or leaves the level source asserted for a later IRQ.
        // It never publishes a deferred lifecycle activation.
        InitIrqProgress::Unhandled
    }

    fn begin_dma_quiesce(
        &mut self,
        epoch: ControllerEpoch,
        cause: RecoveryCause,
    ) -> Result<(), InitError> {
        self.lifecycle.begin_dma_quiesce(&self.shared, epoch, cause)
    }

    fn poll_dma_quiesce(&mut self, input: InitInput) -> InitPoll<DmaQuiesced> {
        let cookie = self.controller_cookie();
        self.lifecycle
            .poll_dma_quiesce(&self.shared, self.config, cookie, input)
    }

    fn enter_guest_owned(&mut self, quiesced: DmaQuiesced) -> Result<(), InitError> {
        let cookie = self.controller_cookie();
        self.lifecycle.enter_guest_owned(cookie, quiesced)
    }

    fn begin_reinitialize(&mut self, quiesced: DmaQuiesced) -> Result<(), InitError> {
        let cookie = self.controller_cookie();
        self.lifecycle
            .begin_reinitialize(&self.shared, cookie, quiesced)
    }

    fn poll_reinitialize(&mut self, input: InitInput) -> InitPoll<ControllerReady> {
        let cookie = self.controller_cookie();
        self.lifecycle
            .poll_reinitialize(&self.shared, self.config, cookie, input)
    }
}

impl Drop for AhciHost {
    fn drop(&mut self) {
        let mut quarantined_dma = self.initialization.quarantine_owned_dma();
        for ready in &mut self.ready_ports {
            if let Some(ready) = ready.take() {
                ready.quarantine();
                quarantined_dma = true;
            }
        }
        if quarantined_dma {
            // Destruction is not a DMA-stop operation. Prevent further IRQ
            // assertions, but retain the mapping and shared anchors because a
            // command/FIS engine may still hold their addresses.
            self.shared.mask_all_ports();
            let ghc = self.shared.registers().read32(HOST_GHC);
            self.shared.registers().write32(HOST_GHC, ghc & !GHC_IE);
            self.shared.set_irq_delivery_enabled(false);
            // The interface disappeared before queue ownership and lifecycle
            // shutdown were established. Keep MMIO and shared state alive so
            // an active FIS engine cannot race mapping destruction.
            mem::forget(Arc::clone(&self.shared));
        }
    }
}

#[cfg(test)]
mod tests {
    use rdif_block::{CompletedRequest, CompletionSink};

    use super::*;
    use crate::{
        ata::AtaDevice,
        command::PortCommandMemory,
        registers::{
            DEFAULT_PORT_IRQ_MASK, GHC_IE, HOST_GHC, MMIO_REQUIRED_SIZE, PX_IE, port_offset,
            read_port, tests_support::FakeRegisters,
        },
        test_support::TEST_DMA,
    };

    #[test]
    fn construction_with_mapped_registers_does_not_access_hardware() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);

        let host = AhciHost::from_test_parts(
            "test-ahci",
            registers.shared(),
            DeviceDma::new_legacy(u64::MAX, &TEST_DMA),
            AhciConfig::legacy_irq(0),
        );

        assert_eq!(
            host.controller_init_state(),
            ControllerInitState::Discovered
        );
        assert!(registers.writes().is_empty());
    }

    #[test]
    fn initial_enable_masks_inherited_global_irq_until_port_ownership_is_known() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        registers.set(HOST_GHC, GHC_IE);
        let host = AhciHost::from_test_parts(
            "test-ahci",
            registers.shared(),
            DeviceDma::new_legacy(u64::MAX, &TEST_DMA),
            AhciConfig::legacy_irq(0),
        );
        let _handler = host.shared.take_initial_handler().unwrap();

        host.enable_irq().unwrap();

        assert!(host.shared.irq_delivery_enabled());
        assert!(
            registers
                .writes()
                .iter()
                .any(|write| write.offset == HOST_GHC && write.value & GHC_IE == 0)
        );
    }

    #[test]
    fn normal_enable_publishes_port_mask_before_global_irq() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let mut host = AhciHost::from_test_parts(
            "test-ahci",
            registers.shared(),
            DeviceDma::new_legacy(u64::MAX, &TEST_DMA),
            AhciConfig::legacy_irq(0),
        );
        host.shared.publish_implemented_ports(1);
        host.shared.publish_ready_port(0);
        host.initialization.mark_ready_for_test();
        let _handler = host.take_irq_handler(0).unwrap();

        host.enable_irq().unwrap();

        let writes = registers.writes();
        let port_enable = writes
            .iter()
            .find(|write| {
                write.offset == port_offset(0, PX_IE) && write.value == DEFAULT_PORT_IRQ_MASK
            })
            .unwrap();
        let global_enable = writes
            .iter()
            .find(|write| write.offset == HOST_GHC && write.value & GHC_IE != 0)
            .unwrap();
        assert!(port_enable.sequence < global_enable.sequence);
    }

    #[test]
    fn normal_enable_refuses_to_unmask_after_the_io_endpoint_is_dropped() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let mut host = AhciHost::from_test_parts(
            "test-ahci",
            registers.shared(),
            DeviceDma::new_legacy(u64::MAX, &TEST_DMA),
            AhciConfig::legacy_irq(0),
        );
        host.shared.publish_implemented_ports(1);
        host.shared.publish_ready_port(0);
        host.initialization.mark_ready_for_test();
        let handler = host.take_irq_handler(0).unwrap();
        drop(handler);

        assert_eq!(
            host.enable_irq(),
            Err(BlkError::Other("AHCI IRQ handler is not live"))
        );
        assert!(registers.writes().is_empty());
        assert!(!host.shared.irq_delivery_enabled());
    }

    #[test]
    fn normal_irq_endpoint_requires_the_initial_endpoint_to_be_released() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let mut host = AhciHost::from_test_parts(
            "test-ahci",
            registers.shared(),
            DeviceDma::new_legacy(u64::MAX, &TEST_DMA),
            AhciConfig::legacy_irq(0),
        );
        let initial_handler = InitialController::take_irq_handler(&mut host, 0)
            .expect("initialization must own the first destructive endpoint");
        host.initialization.mark_ready_for_test();

        assert!(host.take_irq_handler(0).is_none());

        drop(initial_handler);
        assert!(host.take_irq_handler(0).is_some());
    }

    #[test]
    fn separate_ata_ports_publish_independent_device_views_and_queues() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let mut host = AhciHost::from_test_parts(
            "test-ahci-host",
            registers.shared(),
            DeviceDma::new_legacy(u64::MAX, &TEST_DMA),
            AhciConfig::legacy_irq(0),
        );
        install_test_disk(&mut host, 0, 4_096);
        install_test_disk(&mut host, 1, 16_384);
        host.initialization.mark_ready_for_test();

        let available = host.available_port_ids();
        assert!(available.contains(0));
        assert!(available.contains(1));

        let mut disk0 = host
            .take_port_device(0, "test-ahci-disk0")
            .expect("port zero must publish its own disk view");
        let mut disk1 = host
            .take_port_device(1, "test-ahci-disk1")
            .expect("port one must publish its own disk view");
        assert!(host.available_port_ids().is_empty());
        assert_eq!(disk0.device_info().num_blocks, 4_096);
        assert_eq!(disk1.device_info().num_blocks, 16_384);
        assert_eq!(disk0.port_id(), 0);
        assert_eq!(disk1.port_id(), 1);
        assert!(matches!(
            host.take_port_device(0, "duplicate"),
            Err(AhciError::PortUnavailable { port: 0 })
        ));

        let mut queue0 = disk0
            .create_queue()
            .expect("the first disk must own one serialized queue");
        let mut queue1 = disk1
            .create_queue()
            .expect("the second disk must own one serialized queue");
        assert_eq!(queue0.id(), 0);
        assert_eq!(queue1.id(), 1);
        assert_eq!(queue0.info().device.num_blocks, 4_096);
        assert_eq!(queue1.info().device.num_blocks, 16_384);
        assert_eq!(disk0.device_info().num_blocks, 4_096);
        assert_eq!(disk1.device_info().num_blocks, 16_384);
        assert!(disk0.queue_limits().supports_flush);
        assert!(disk1.queue_limits().supports_flush);
        assert!(disk0.create_queue().is_none());
        assert!(disk1.create_queue().is_none());

        disk0.shared.port(0).set_online(false);
        disk1.shared.port(1).set_online(false);
        let mut sink = RejectCompletion;
        queue0.shutdown(&mut sink).unwrap();
        queue1.shutdown(&mut sink).unwrap();
    }

    #[test]
    fn abandoned_port_device_masks_irq_and_quarantines_live_command_memory() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let mut host = AhciHost::from_test_parts(
            "test-ahci-host",
            registers.shared(),
            DeviceDma::new_legacy(u64::MAX, &TEST_DMA),
            AhciConfig::legacy_irq(0),
        );
        install_test_disk(&mut host, 3, 4_096);
        host.initialization.mark_ready_for_test();
        registers.set(port_offset(3, PX_IE), DEFAULT_PORT_IRQ_MASK);
        let disk = host
            .take_port_device(3, "test-ahci-disk3")
            .expect("the test port must be extractable");

        drop(disk);

        assert_eq!(read_port(registers.as_ref(), 3, PX_IE), 0);
        assert!(!host.shared.port(3).is_online());
    }

    fn install_test_disk(host: &mut AhciHost, port: usize, num_blocks: u64) {
        let command_memory = PortCommandMemory::allocate(&host.dma).unwrap();
        host.shared
            .publish_implemented_ports(host.shared.implemented_ports() | (1_u32 << port));
        host.shared.publish_ready_port(port);
        host.ready_ports.push(Some(ReadyPort {
            port,
            ata: AtaDevice {
                num_blocks,
                logical_block_size: 512,
                lba48: true,
                flush: true,
            },
            command_memory,
        }));
    }

    struct RejectCompletion;

    impl CompletionSink for RejectCompletion {
        fn complete(&mut self, completion: CompletedRequest) {
            panic!("idle test queue returned unexpected completion: {completion:?}");
        }
    }
}
