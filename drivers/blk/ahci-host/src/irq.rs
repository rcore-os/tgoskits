use alloc::{boxed::Box, sync::Arc};
use core::{
    array,
    cell::UnsafeCell,
    mem::MaybeUninit,
    num::NonZeroU64,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering, fence},
};

use rdif_block::{
    BlkError, BlockEvidenceSource, BlockIrqCapture, BlockIrqSource, ContainmentCause, Event,
    FaultContainment, IrqCapture, IrqControlError, IrqEndpoint, IrqEvidenceId, IrqSourceControl,
    IrqSourceId, MaskedSource,
};

use crate::{
    evidence::{AhciEvidenceError, AhciEvidenceLedger},
    registers::{
        DEFAULT_PORT_IRQ_MASK, HOST_IS, IRQ_COMPLETION, IRQ_ERROR, MAX_PORTS, PX_CI, PX_IE, PX_IS,
        PX_SACT, PX_SERR, PX_TFD, SharedRegisters, TFD_ERR, read_port, write_port,
    },
};

pub(crate) const IRQ_SNAPSHOT_CAPACITY: usize = 64;
const REARMING_MASK_EPOCH: u64 = u64::MAX;

/// Stable register state captured by the unique destructive IRQ endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PortIrqSnapshot {
    pub epoch: u64,
    pub request_generation: u64,
    pub status: u32,
    pub command_issue: u32,
    pub sata_active: u32,
    pub task_file: u32,
    pub sata_error: u32,
}

impl PortIrqSnapshot {
    pub(crate) const fn has_error(self) -> bool {
        self.status & IRQ_ERROR != 0 || self.task_file & TFD_ERR != 0 || self.sata_error != 0
    }

    pub(crate) const fn completes(self, slot: usize, request_generation: u64) -> bool {
        request_generation != 0
            && self.request_generation == request_generation
            && self.status & IRQ_COMPLETION != 0
            && self.command_issue & (1_u32 << slot) == 0
    }
}

pub(crate) struct PortShared {
    snapshots: SnapshotRing,
    overflow: AtomicBool,
    epoch: AtomicU64,
    next_request_generation: AtomicU64,
    active_request_generation: AtomicU64,
    online: AtomicBool,
    command_list_dma: AtomicU64,
    received_fis_dma: AtomicU64,
    dma_bases_valid: AtomicBool,
}

impl PortShared {
    fn new() -> Self {
        Self {
            snapshots: SnapshotRing::new(),
            overflow: AtomicBool::new(false),
            epoch: AtomicU64::new(1),
            next_request_generation: AtomicU64::new(0),
            active_request_generation: AtomicU64::new(0),
            online: AtomicBool::new(false),
            command_list_dma: AtomicU64::new(0),
            received_fis_dma: AtomicU64::new(0),
            dma_bases_valid: AtomicBool::new(false),
        }
    }

    pub(crate) fn pop_snapshot(&self) -> Option<PortIrqSnapshot> {
        self.snapshots.pop()
    }

    pub(crate) fn has_snapshots(&self) -> bool {
        !self.snapshots.is_empty()
    }

    pub(crate) fn take_overflow(&self) -> bool {
        self.overflow.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    pub(crate) fn publish_epoch(&self, epoch: u64) {
        self.epoch.store(epoch, Ordering::Release);
    }

    pub(crate) fn next_request_generation(&self) -> u64 {
        let previous = self.next_request_generation.fetch_add(1, Ordering::Relaxed);
        let generation = previous.wrapping_add(1);
        if generation == 0 {
            self.next_request_generation.store(1, Ordering::Relaxed);
            1
        } else {
            generation
        }
    }

    pub(crate) fn active_request_generation(&self) -> u64 {
        self.active_request_generation.load(Ordering::Acquire)
    }

    pub(crate) fn publish_active_request(&self, generation: u64) -> bool {
        generation != 0
            && self
                .active_request_generation
                .compare_exchange(0, generation, Ordering::Release, Ordering::Acquire)
                .is_ok()
    }

    pub(crate) fn clear_active_request(&self, generation: u64) -> bool {
        generation != 0
            && self
                .active_request_generation
                .compare_exchange(generation, 0, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }

    pub(crate) fn clear_any_active_request(&self) {
        self.active_request_generation.store(0, Ordering::Release);
    }

    pub(crate) fn is_online(&self) -> bool {
        self.online.load(Ordering::Acquire)
    }

    pub(crate) fn set_online(&self, online: bool) {
        self.online.store(online, Ordering::Release);
    }

    pub(crate) fn publish_dma_bases(&self, command_list: u64, received_fis: u64) {
        self.command_list_dma.store(command_list, Ordering::Relaxed);
        self.received_fis_dma.store(received_fis, Ordering::Relaxed);
        self.dma_bases_valid.store(true, Ordering::Release);
    }

    pub(crate) fn dma_bases(&self) -> Option<(u64, u64)> {
        if !self.dma_bases_valid.load(Ordering::Acquire) {
            return None;
        }
        let received_fis = self.received_fis_dma.load(Ordering::Acquire);
        let command_list = self.command_list_dma.load(Ordering::Relaxed);
        Some((command_list, received_fis))
    }

    pub(crate) fn discard_stale_snapshots(&self) {
        while self.snapshots.pop().is_some() {}
        self.overflow.store(false, Ordering::Release);
    }
}

pub(crate) struct HostShared {
    registers: SharedRegisters,
    ports: [PortShared; MAX_PORTS],
    implemented_ports: AtomicU32,
    ready_ports: AtomicU32,
    masked_ports: AtomicU32,
    source_generation: AtomicU64,
    irq_delivery_enabled: AtomicBool,
    capture_active: AtomicBool,
    init_handler_taken: AtomicBool,
    init_handler_live: AtomicBool,
    io_handler_taken: AtomicBool,
    io_handler_live: AtomicBool,
    v13_handler_taken: AtomicBool,
    v13_handler_live: AtomicBool,
    next_mask_epoch: AtomicU64,
    active_mask_epoch: AtomicU64,
}

impl HostShared {
    pub(crate) fn new(registers: SharedRegisters) -> Arc<Self> {
        Arc::new(Self {
            registers,
            ports: array::from_fn(|_| PortShared::new()),
            implemented_ports: AtomicU32::new(0),
            ready_ports: AtomicU32::new(0),
            masked_ports: AtomicU32::new(0),
            source_generation: AtomicU64::new(1),
            irq_delivery_enabled: AtomicBool::new(false),
            capture_active: AtomicBool::new(false),
            init_handler_taken: AtomicBool::new(false),
            init_handler_live: AtomicBool::new(false),
            io_handler_taken: AtomicBool::new(false),
            io_handler_live: AtomicBool::new(false),
            v13_handler_taken: AtomicBool::new(false),
            v13_handler_live: AtomicBool::new(false),
            next_mask_epoch: AtomicU64::new(0),
            active_mask_epoch: AtomicU64::new(0),
        })
    }

    pub(crate) fn registers(&self) -> &dyn crate::registers::RegisterIo {
        self.registers.as_ref()
    }

    pub(crate) fn port(&self, port: usize) -> &PortShared {
        &self.ports[port]
    }

    pub(crate) fn publish_implemented_ports(&self, ports: u32) {
        self.implemented_ports.store(ports, Ordering::Release);
    }

    pub(crate) fn implemented_ports(&self) -> u32 {
        self.implemented_ports.load(Ordering::Acquire)
    }

    pub(crate) fn publish_ready_port(&self, port: usize) {
        self.ready_ports.fetch_or(1 << port, Ordering::Release);
        self.port(port).set_online(true);
    }

    pub(crate) fn ready_ports(&self) -> u32 {
        self.ready_ports.load(Ordering::Acquire)
    }

    pub(crate) fn set_irq_delivery_enabled(&self, enabled: bool) {
        let previous = self.irq_delivery_enabled.swap(enabled, Ordering::AcqRel);
        if enabled && !previous {
            let mut generation = self
                .source_generation
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1);
            if generation == 0 {
                self.source_generation.store(1, Ordering::Release);
                generation = 1;
            }
            debug_assert_ne!(generation, 0);
        }
    }

    pub(crate) fn irq_delivery_enabled(&self) -> bool {
        self.irq_delivery_enabled.load(Ordering::Acquire)
    }

    pub(crate) fn take_initial_source(self: &Arc<Self>) -> Option<BlockIrqSource> {
        self.init_handler_taken
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        self.init_handler_live.store(true, Ordering::Release);
        Some(self.new_irq_source(IrqEndpointRole::Initialization))
    }

    pub(crate) fn initial_handler_live(&self) -> bool {
        self.init_handler_live.load(Ordering::Acquire)
    }

    pub(crate) fn control_handler_live(&self) -> bool {
        self.initial_handler_live() || self.v13_handler_live.load(Ordering::Acquire)
    }

    pub(crate) fn v13_handler_live(&self) -> bool {
        self.v13_handler_live.load(Ordering::Acquire)
    }

    pub(crate) fn take_io_source(self: &Arc<Self>) -> Option<BlockIrqSource> {
        if self.initial_handler_live() {
            return None;
        }
        self.io_handler_taken
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        self.io_handler_live.store(true, Ordering::Release);
        Some(self.new_irq_source(IrqEndpointRole::NormalIo))
    }

    pub(crate) fn io_handler_live(&self) -> bool {
        self.io_handler_live.load(Ordering::Acquire)
    }

    /// Transfers the one v0.13 shared-source endpoint and its fixed ledger.
    pub(crate) fn take_v13_source(
        self: &Arc<Self>,
        source: IrqSourceId,
    ) -> Option<(BlockEvidenceSource, Arc<AhciEvidenceLedger>)> {
        if self.initial_handler_live() || self.io_handler_live() {
            return None;
        }
        self.v13_handler_taken
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        self.v13_handler_live.store(true, Ordering::Release);
        let ledger = Arc::new(AhciEvidenceLedger::new(source, 0));
        let endpoint = AhciEvidenceEndpoint {
            shared: Arc::clone(self),
            source,
            ledger: Arc::clone(&ledger),
        };
        let control = AhciEvidenceIrqControl {
            shared: Arc::clone(self),
            source,
        };
        Some((
            BlockEvidenceSource::new(Box::new(endpoint), Box::new(control)),
            ledger,
        ))
    }

    pub(crate) fn try_claim_register_window(&self) -> Option<CaptureGuard<'_>> {
        CaptureGuard::try_acquire(&self.capture_active)
    }

    fn new_irq_source(self: &Arc<Self>, role: IrqEndpointRole) -> BlockIrqSource {
        BlockIrqSource::new(
            Box::new(AhciIrqHandler::new(Arc::clone(self), role)),
            Box::new(AhciIrqControl {
                shared: Arc::clone(self),
            }),
        )
    }

    fn capture_irq(&self) -> BlockIrqCapture {
        if !self.irq_delivery_enabled() {
            return IrqCapture::Unhandled;
        }
        let Some(_capture) = CaptureGuard::try_acquire(&self.capture_active) else {
            return IrqCapture::Fault {
                reason: BlkError::Busy,
                containment: FaultContainment::Uncontained,
            };
        };
        self.capture_irq_facts()
    }

    /// Captures v0.13 facts through its unique, fixed-CPU IRQ endpoint.
    ///
    /// Unlike the retained compatibility endpoint, this path never shares a
    /// register-window try-lock with task-side submission. The bound endpoint
    /// is the only PxIS/PxSERR W1C owner and the runtime serializes action
    /// disable/synchronize with controller lifecycle operations.
    fn capture_v13_irq(&self) -> BlockIrqCapture {
        if !self.irq_delivery_enabled() {
            return IrqCapture::Unhandled;
        }
        self.capture_irq_facts()
    }

    fn capture_irq_facts(&self) -> BlockIrqCapture {
        let host_status = self.registers.read32(HOST_IS);
        if host_status == 0 {
            return IrqCapture::Unhandled;
        }

        let pending_ports = host_status & self.implemented_ports();
        let mut event = Event::none();
        let mut masked_ports = 0_u32;
        for port in 0..MAX_PORTS {
            if pending_ports & (1 << port) == 0 {
                continue;
            }
            let Some(masked) = self.capture_port_irq(port) else {
                continue;
            };
            event.push_queue(port);
            if masked {
                masked_ports |= 1 << port;
            }
        }

        // AHCI host status is a level-triggered latch. Every port status must
        // be acknowledged before the unmasked host value is cleared.
        self.registers.write32(HOST_IS, host_status);
        IrqCapture::Captured {
            event,
            masked: (masked_ports != 0).then(|| self.masked_source(masked_ports)),
        }
    }

    fn capture_port_irq(&self, port: usize) -> Option<bool> {
        let status = read_port(self.registers(), port, PX_IS);
        if status == 0 {
            return None;
        }

        let sata_error = if status & IRQ_ERROR != 0 {
            read_port(self.registers(), port, PX_SERR)
        } else {
            0
        };
        let snapshot = PortIrqSnapshot {
            epoch: self.port(port).epoch(),
            request_generation: self.port(port).active_request_generation(),
            status,
            command_issue: read_port(self.registers(), port, PX_CI),
            sata_active: read_port(self.registers(), port, PX_SACT),
            task_file: read_port(self.registers(), port, PX_TFD),
            sata_error,
        };

        if sata_error != 0 {
            write_port(self.registers(), port, PX_SERR, sata_error);
        }
        write_port(self.registers(), port, PX_IS, status);

        let masked = if !self.port(port).snapshots.push(snapshot) {
            self.port(port).overflow.store(true, Ordering::Release);
            // Overflow loses stable device facts, so freeze this port and let
            // the bounded owner enter controller recovery.
            self.mask_port(port);
            true
        } else {
            false
        };
        Some(masked)
    }

    pub(crate) fn mask_all_ports(&self) {
        let implemented = self.implemented_ports();
        for port in 0..MAX_PORTS {
            if implemented & (1 << port) != 0 {
                write_port(self.registers(), port, PX_IE, 0);
            }
        }
        self.masked_ports.fetch_or(implemented, Ordering::Release);
    }

    pub(crate) fn mask_port(&self, port: usize) {
        write_port(self.registers(), port, PX_IE, 0);
        self.masked_ports.fetch_or(1 << port, Ordering::Release);
    }

    pub(crate) fn unmask_ready_ports(&self) {
        let ready = self.ready_ports();
        for port in 0..MAX_PORTS {
            if ready & (1 << port) != 0 {
                write_port(self.registers(), port, PX_IE, DEFAULT_PORT_IRQ_MASK);
            }
        }
        self.masked_ports.fetch_and(!ready, Ordering::Release);
    }

    fn masked_source(&self, ports: u32) -> MaskedSource {
        let generation = NonZeroU64::new(self.source_generation.load(Ordering::Acquire))
            .expect("AHCI IRQ source generation is always nonzero");
        let bitmap = NonZeroU64::new(u64::from(ports))
            .expect("AHCI masked source always owns at least one port");
        MaskedSource::new(generation, bitmap)
    }

    fn contain_source(&self, _cause: ContainmentCause) -> Result<MaskedSource, BlkError> {
        let _capture = self.try_claim_register_window().ok_or(BlkError::Busy)?;
        let ports = self.implemented_ports();
        if ports == 0 {
            return Err(BlkError::Other("AHCI has no maskable implemented port"));
        }
        self.mask_all_ports();
        Ok(self.masked_source(ports))
    }

    fn mask_v13_source(&self, source: IrqSourceId) -> Result<MaskedSource, BlkError> {
        let source_bitmap = 1_u64
            .checked_shl(source.get() as u32)
            .and_then(NonZeroU64::new)
            .ok_or(BlkError::InvalidRequest)?;
        let lifecycle_generation = NonZeroU64::new(self.source_generation.load(Ordering::Acquire))
            .ok_or(BlkError::Other("AHCI source lifecycle generation is zero"))?;
        let ghc = self.registers.read32(crate::registers::HOST_GHC);
        self.registers
            .write32(crate::registers::HOST_GHC, ghc & !crate::registers::GHC_IE);
        let mask_epoch = loop {
            let observed = self.active_mask_epoch.load(Ordering::Acquire);
            if observed != 0 && observed != REARMING_MASK_EPOCH {
                break observed;
            }
            let next = self.allocate_mask_epoch();
            if self
                .active_mask_epoch
                .compare_exchange(observed, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break next;
            }
        };
        let mask_epoch =
            NonZeroU64::new(mask_epoch).ok_or(BlkError::Other("AHCI source mask epoch is zero"))?;
        Ok(MaskedSource::new_with_epoch(
            lifecycle_generation,
            mask_epoch,
            source_bitmap,
        ))
    }

    fn allocate_mask_epoch(&self) -> u64 {
        loop {
            let next = self
                .next_mask_epoch
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1);
            if next != 0 && next != REARMING_MASK_EPOCH {
                return next;
            }
        }
    }

    fn rearm_v13_source(
        &self,
        source_id: IrqSourceId,
        source: MaskedSource,
    ) -> Result<(), IrqControlError> {
        let expected_lifecycle = self.source_generation.load(Ordering::Acquire);
        if source.lifecycle_generation().get() != expected_lifecycle {
            return Err(IrqControlError::StaleGeneration {
                expected: expected_lifecycle,
                actual: source.lifecycle_generation().get(),
            });
        }
        let expected_epoch = source.mask_epoch().get();
        let expected_bitmap =
            1_u64
                .checked_shl(source_id.get() as u32)
                .ok_or(IrqControlError::SourceNotMasked {
                    bitmap: source.bitmap().get(),
                })?;
        if source.bitmap().get() != expected_bitmap {
            return Err(IrqControlError::SourceNotMasked {
                bitmap: source.bitmap().get(),
            });
        }
        // Mark the old episode as being rearmed before opening the hardware
        // gate. A shared-line callback that interrupts this transaction can
        // replace the sentinel with a new mask epoch; the tail then observes
        // that episode and leaves the source disabled.
        self.active_mask_epoch
            .compare_exchange(
                expected_epoch,
                REARMING_MASK_EPOCH,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|current| IrqControlError::StaleMaskEpoch {
                expected: current,
                actual: source.mask_epoch().get(),
            })?;
        let ghc = self.registers.read32(crate::registers::HOST_GHC);
        self.registers.write32(
            crate::registers::HOST_GHC,
            ghc | crate::registers::GHC_AE | crate::registers::GHC_IE,
        );
        if self
            .active_mask_epoch
            .compare_exchange(REARMING_MASK_EPOCH, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            let ghc = self.registers.read32(crate::registers::HOST_GHC);
            self.registers
                .write32(crate::registers::HOST_GHC, ghc & !crate::registers::GHC_IE);
        }
        Ok(())
    }

    fn rearm_source(&self, source: MaskedSource) -> Result<(), IrqControlError> {
        let generation = source.generation().get();
        let active = self.source_generation.load(Ordering::Acquire);
        if generation != active {
            return Err(IrqControlError::StaleGeneration {
                expected: active,
                actual: generation,
            });
        }
        let bitmap = source.bitmap().get();
        let ports =
            u32::try_from(bitmap).map_err(|_| IrqControlError::SourceNotMasked { bitmap })?;
        let masked = self.masked_ports.load(Ordering::Acquire);
        if ports == 0 || ports & !masked != 0 {
            return Err(IrqControlError::SourceNotMasked { bitmap });
        }
        if ports & !self.ready_ports() != 0 {
            return Err(IrqControlError::Offline);
        }
        let _capture = self
            .try_claim_register_window()
            .ok_or(IrqControlError::Hardware(BlkError::Busy))?;
        for port in 0..MAX_PORTS {
            if ports & (1 << port) != 0 {
                write_port(self.registers(), port, PX_IE, DEFAULT_PORT_IRQ_MASK);
            }
        }
        self.masked_ports.fetch_and(!ports, Ordering::Release);
        Ok(())
    }
}

pub(crate) struct CaptureGuard<'active> {
    active: &'active AtomicBool,
}

impl<'active> CaptureGuard<'active> {
    fn try_acquire(active: &'active AtomicBool) -> Option<Self> {
        active
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
            .then_some(Self { active })
    }
}

impl Drop for CaptureGuard<'_> {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

struct AhciIrqHandler {
    shared: Arc<HostShared>,
    role: IrqEndpointRole,
}

impl AhciIrqHandler {
    fn new(shared: Arc<HostShared>, role: IrqEndpointRole) -> Self {
        Self { shared, role }
    }
}

impl IrqEndpoint for AhciIrqHandler {
    type Event = Event;
    type Fault = BlkError;

    fn capture(&mut self) -> BlockIrqCapture {
        self.shared.capture_irq()
    }

    fn contain(&mut self, cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
        self.shared.contain_source(cause)
    }
}

struct AhciIrqControl {
    shared: Arc<HostShared>,
}

struct AhciEvidenceEndpoint {
    shared: Arc<HostShared>,
    source: IrqSourceId,
    ledger: Arc<AhciEvidenceLedger>,
}

impl IrqEndpoint for AhciEvidenceEndpoint {
    type Event = IrqEvidenceId;
    type Fault = BlkError;

    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault> {
        let port_facts = match self.shared.capture_v13_irq() {
            IrqCapture::Unhandled => return IrqCapture::Unhandled,
            IrqCapture::Captured { event, .. } if !event.is_empty() => event.queues().bits(),
            IrqCapture::Captured { .. } => {
                let containment = match self.shared.mask_v13_source(self.source) {
                    Ok(masked) => FaultContainment::DeviceSourceMasked(masked),
                    Err(_) => FaultContainment::Uncontained,
                };
                return IrqCapture::Fault {
                    reason: BlkError::Io,
                    containment,
                };
            }
            IrqCapture::Fault {
                reason,
                containment,
            } => {
                return IrqCapture::Fault {
                    reason,
                    containment,
                };
            }
        };
        let masked = match self.shared.mask_v13_source(self.source) {
            Ok(masked) => masked,
            Err(reason) => {
                return IrqCapture::Fault {
                    reason,
                    containment: FaultContainment::Uncontained,
                };
            }
        };
        let port_facts = match u32::try_from(port_facts) {
            Ok(port_facts) => port_facts,
            Err(_) => {
                return IrqCapture::Fault {
                    reason: BlkError::InvalidRequest,
                    containment: FaultContainment::DeviceSourceMasked(masked),
                };
            }
        };
        match self
            .ledger
            .publish(masked.lifecycle_generation(), port_facts)
        {
            Ok(event) => IrqCapture::Captured {
                event,
                masked: Some(masked),
            },
            Err(error) => IrqCapture::Fault {
                reason: evidence_error(error),
                containment: FaultContainment::DeviceSourceMasked(masked),
            },
        }
    }

    fn contain(&mut self, _cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
        self.shared.mask_v13_source(self.source)
    }
}

impl Drop for AhciEvidenceEndpoint {
    fn drop(&mut self) {
        self.shared.v13_handler_live.store(false, Ordering::Release);
    }
}

struct AhciEvidenceIrqControl {
    shared: Arc<HostShared>,
    source: IrqSourceId,
}

impl IrqSourceControl for AhciEvidenceIrqControl {
    type Error = IrqControlError;

    fn rearm(&mut self, source: MaskedSource) -> Result<(), Self::Error> {
        self.shared.rearm_v13_source(self.source, source)
    }
}

const fn evidence_error(error: AhciEvidenceError) -> BlkError {
    match error {
        AhciEvidenceError::EmptyFacts | AhciEvidenceError::IdentityMismatch => {
            BlkError::InvalidRequest
        }
        AhciEvidenceError::GenerationExhausted
        | AhciEvidenceError::LifecycleConflict
        | AhciEvidenceError::NotPublished
        | AhciEvidenceError::PublicationInProgress => BlkError::Io,
    }
}

impl IrqSourceControl for AhciIrqControl {
    type Error = IrqControlError;

    fn rearm(&mut self, source: MaskedSource) -> Result<(), Self::Error> {
        self.shared.rearm_source(source)
    }
}

impl Drop for AhciIrqHandler {
    fn drop(&mut self) {
        self.role
            .live_flag(&self.shared)
            .store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy)]
enum IrqEndpointRole {
    Initialization,
    NormalIo,
}

impl IrqEndpointRole {
    fn live_flag(self, shared: &HostShared) -> &AtomicBool {
        match self {
            Self::Initialization => &shared.init_handler_live,
            Self::NormalIo => &shared.io_handler_live,
        }
    }
}

struct SnapshotRing {
    slots: [UnsafeCell<MaybeUninit<PortIrqSnapshot>>; IRQ_SNAPSHOT_CAPACITY],
    head: AtomicUsize,
    tail: AtomicUsize,
}

// SAFETY: the retained endpoint uses `HostShared::capture_active`, while the
// v0.13 endpoint is a unique action fixed to one CPU. Both contracts permit one
// effective producer; the ownership-domain maintenance thread is the sole
// consumer. Release publication of `head` happens after slot initialization;
// Acquire observation happens before the consumer read.
unsafe impl Sync for SnapshotRing {}

impl SnapshotRing {
    fn new() -> Self {
        Self {
            slots: array::from_fn(|_| UnsafeCell::new(MaybeUninit::uninit())),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    fn push(&self, snapshot: PortIrqSnapshot) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) >= IRQ_SNAPSHOT_CAPACITY {
            return false;
        }
        let index = head % IRQ_SNAPSHOT_CAPACITY;
        unsafe {
            // SAFETY: the HBA capture gate permits only one effective producer
            // to write this slot, and the capacity check proves the consumer
            // has released its old value.
            (*self.slots[index].get()).write(snapshot);
        }
        self.head.store(head.wrapping_add(1), Ordering::Release);
        true
    }

    fn pop(&self) -> Option<PortIrqSnapshot> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail == head {
            return None;
        }
        let index = tail % IRQ_SNAPSHOT_CAPACITY;
        let snapshot = unsafe {
            // SAFETY: Acquire observed the producer's initialized slot, and
            // only the single serialized consumer reads or advances `tail`.
            (*self.slots[index].get()).assume_init_read()
        };
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some(snapshot)
    }

    fn is_empty(&self) -> bool {
        fence(Ordering::Acquire);
        self.tail.load(Ordering::Relaxed) == self.head.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::registers::{
        HOST_IS, IRQ_D2H_REG_FIS, IRQ_TASK_FILE_ERROR, MMIO_REQUIRED_SIZE, PX_CI, PX_IE, PX_IS,
        PX_SERR, PX_TFD, TFD_ERR, port_offset, tests_support::FakeRegisters,
    };

    #[test]
    fn error_and_completion_in_one_irq_is_preserved_as_error_snapshot() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        shared.publish_implemented_ports(1);
        shared.set_irq_delivery_enabled(true);
        let generation = shared.port(0).next_request_generation();
        assert!(shared.port(0).publish_active_request(generation));
        registers.set(HOST_IS, 1);
        registers.set(port_offset(0, PX_IS), IRQ_TASK_FILE_ERROR | IRQ_D2H_REG_FIS);
        registers.set(port_offset(0, PX_CI), 0);
        registers.set(port_offset(0, PX_TFD), TFD_ERR);
        registers.set(port_offset(0, PX_SERR), 0x40);

        let outcome = shared.capture_irq();
        let snapshot = shared.port(0).pop_snapshot().unwrap();

        assert!(outcome.is_captured());
        assert!(snapshot.has_error());
        assert!(snapshot.completes(0, generation));
        assert_eq!(snapshot.sata_error, 0x40);
    }

    #[test]
    fn v13_shared_irq_claims_only_real_ahci_status_and_masks_the_logical_source() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        shared.publish_implemented_ports(1);
        shared.publish_ready_port(0);
        shared.set_irq_delivery_enabled(true);
        let source_id = rdif_block::IrqSourceId::new(3).unwrap();
        let (source, _ledger) = shared
            .take_v13_source(source_id)
            .expect("one v0.13 source must be transferable");
        let (mut endpoint, mut control) = source.into_parts();

        assert!(endpoint.capture().is_unhandled());

        registers.set(HOST_IS, 1);
        registers.set(port_offset(0, PX_IS), IRQ_D2H_REG_FIS);
        let IrqCapture::Captured { event, masked } = endpoint.capture() else {
            panic!("programmed AHCI status must create linear evidence")
        };
        assert_eq!(event.source(), source_id);
        assert_eq!(event.slot(), 0);
        let first_mask = masked.unwrap();
        assert_eq!(first_mask.bitmap().get(), 1 << source_id.get());
        control.rearm(first_mask).unwrap();

        let second_mask = endpoint
            .contain(ContainmentCause::OwnerUnavailable)
            .expect("the live AHCI source must fail closed");
        assert_ne!(first_mask.mask_epoch(), second_mask.mask_epoch());
        assert!(matches!(
            control.rearm(first_mask),
            Err(IrqControlError::StaleMaskEpoch { .. })
        ));
        control.rearm(second_mask).unwrap();
    }

    #[test]
    fn v13_irq_capture_is_independent_from_the_legacy_register_gate() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        shared.publish_implemented_ports(1);
        shared.publish_ready_port(0);
        shared.set_irq_delivery_enabled(true);
        let source_id = rdif_block::IrqSourceId::new(3).unwrap();
        let (source, _ledger) = shared
            .take_v13_source(source_id)
            .expect("one v0.13 source must be transferable");
        let (mut endpoint, _control) = source.into_parts();
        let _legacy_window = shared
            .try_claim_register_window()
            .expect("the legacy compatibility gate starts available");
        registers.set(HOST_IS, 1);
        registers.set(port_offset(0, PX_IS), IRQ_D2H_REG_FIS);

        assert!(
            endpoint.capture().is_captured(),
            "v0.13 hard IRQ must capture hardware facts instead of reporting lock contention"
        );
    }

    #[test]
    fn v13_capture_during_rearm_replaces_the_consumed_mask_epoch() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        shared.publish_implemented_ports(1);
        shared.publish_ready_port(0);
        shared.set_irq_delivery_enabled(true);
        let source_id = rdif_block::IrqSourceId::new(3).unwrap();
        let (source, _ledger) = shared.take_v13_source(source_id).unwrap();
        let (mut endpoint, mut control) = source.into_parts();
        let old = endpoint
            .contain(ContainmentCause::OwnerUnavailable)
            .unwrap();
        assert!(
            shared
                .active_mask_epoch
                .compare_exchange(
                    old.mask_epoch().get(),
                    REARMING_MASK_EPOCH,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        );

        let raced = endpoint
            .contain(ContainmentCause::OwnerUnavailable)
            .unwrap();

        assert_ne!(old.mask_epoch(), raced.mask_epoch());
        assert_ne!(raced.mask_epoch().get(), REARMING_MASK_EPOCH);
        assert!(matches!(
            control.rearm(old),
            Err(IrqControlError::StaleMaskEpoch { .. })
        ));
        control.rearm(raced).unwrap();
    }

    #[test]
    fn irq_acknowledges_port_status_before_host_latch() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        shared.publish_implemented_ports(1);
        shared.set_irq_delivery_enabled(true);
        registers.set(HOST_IS, 1);
        registers.set(port_offset(0, PX_IS), IRQ_D2H_REG_FIS);

        assert!(shared.capture_irq().is_captured());

        let writes = registers.writes();
        let port_ack = writes
            .iter()
            .find(|write| write.offset == port_offset(0, PX_IS))
            .unwrap();
        let host_ack = writes.iter().find(|write| write.offset == HOST_IS).unwrap();
        assert!(port_ack.sequence < host_ack.sequence);
    }

    #[test]
    fn capture_gate_contention_leaves_the_level_source_for_a_new_hard_irq() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        shared.publish_implemented_ports(1);
        shared.set_irq_delivery_enabled(true);
        registers.set(HOST_IS, 1);
        registers.set(port_offset(0, PX_IS), IRQ_D2H_REG_FIS);

        let capture = CaptureGuard::try_acquire(&shared.capture_active).unwrap();
        let contended = shared.capture_irq();
        assert!(contended.is_fault());
        assert!(registers.writes().is_empty());

        drop(capture);
        let retried = shared.capture_irq();
        assert!(retried.is_captured());
    }

    #[test]
    fn shared_irq_routes_each_port_to_its_independent_generation() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        shared.publish_implemented_ports(0b11);
        shared.set_irq_delivery_enabled(true);

        let _superseded_port_zero_generation = shared.port(0).next_request_generation();
        let port_zero_generation = shared.port(0).next_request_generation();
        let port_one_generation = shared.port(1).next_request_generation();
        assert_eq!(port_zero_generation, 2);
        assert_eq!(port_one_generation, 1);
        assert!(shared.port(0).publish_active_request(port_zero_generation));
        assert!(shared.port(1).publish_active_request(port_one_generation));

        registers.set(HOST_IS, 0b11);
        for port in [0, 1] {
            registers.set(port_offset(port, PX_IS), IRQ_D2H_REG_FIS);
            registers.set(port_offset(port, PX_CI), 0);
        }

        let outcome = shared.capture_irq();
        let (event, _masked) = outcome
            .captured()
            .expect("programmed AHCI status must be captured");
        assert!(event.for_queue(0).is_some());
        assert!(event.for_queue(1).is_some());

        let port_zero = shared.port(0).pop_snapshot().unwrap();
        let port_one = shared.port(1).pop_snapshot().unwrap();
        assert_eq!(port_zero.request_generation, port_zero_generation);
        assert_eq!(port_one.request_generation, port_one_generation);
        assert!(port_zero.completes(0, port_zero_generation));
        assert!(port_one.completes(0, port_one_generation));
        assert!(!port_zero.completes(0, port_one_generation));
        assert!(!port_one.completes(0, port_zero_generation));
    }

    #[test]
    fn snapshot_overflow_masks_the_port_instead_of_losing_events_silently() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        shared.publish_implemented_ports(1);
        shared.set_irq_delivery_enabled(true);
        for _ in 0..=IRQ_SNAPSHOT_CAPACITY {
            registers.set(HOST_IS, 1);
            registers.set(port_offset(0, PX_IS), IRQ_D2H_REG_FIS);
            let _ = shared.capture_irq();
        }

        assert!(shared.port(0).take_overflow());
        assert!(
            registers
                .writes()
                .iter()
                .any(|write| { write.offset == port_offset(0, PX_IE) && write.value == 0 })
        );
    }

    #[test]
    fn completion_latched_while_globally_masked_is_bound_to_the_armed_generation() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        shared.publish_implemented_ports(1);
        let generation = shared.port(0).next_request_generation();
        assert!(shared.port(0).publish_active_request(generation));

        registers.set(HOST_IS, 1);
        registers.set(port_offset(0, PX_IS), IRQ_D2H_REG_FIS);
        registers.set(port_offset(0, PX_CI), 0);
        assert!(shared.capture_irq().is_unhandled());

        shared.set_irq_delivery_enabled(true);
        assert!(shared.capture_irq().is_captured());
        let snapshot = shared.port(0).pop_snapshot().unwrap();
        assert!(snapshot.completes(0, generation));

        assert!(shared.port(0).clear_active_request(generation));
        let next_generation = shared.port(0).next_request_generation();
        assert!(shared.port(0).publish_active_request(next_generation));
        assert!(!snapshot.completes(0, next_generation));
    }

    #[test]
    fn containment_token_rearms_only_the_matching_controller_generation() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        shared.publish_implemented_ports(1);
        shared.publish_ready_port(0);
        shared.set_irq_delivery_enabled(true);
        let (mut endpoint, mut control) = shared.take_initial_source().unwrap().into_parts();

        let masked = endpoint
            .contain(ContainmentCause::PublicationFull)
            .expect("AHCI port source must be precisely containable");
        assert_eq!(masked.bitmap().get(), 1);
        control
            .rearm(masked)
            .expect("matching generation must reopen the masked port");
        assert!(registers.writes().iter().any(|write| {
            write.offset == port_offset(0, PX_IE) && write.value == DEFAULT_PORT_IRQ_MASK
        }));

        let stale = endpoint
            .contain(ContainmentCause::OwnerUnavailable)
            .expect("the same live epoch remains containable");
        shared.set_irq_delivery_enabled(false);
        shared.set_irq_delivery_enabled(true);
        assert!(matches!(
            control.rearm(stale),
            Err(IrqControlError::StaleGeneration { .. })
        ));
    }
}
