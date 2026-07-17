use alloc::{boxed::Box, sync::Arc};
use core::{
    array,
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering, fence},
};

use rdif_block::{Event, IrqHandler, IrqOutcome};

use crate::registers::{
    DEFAULT_PORT_IRQ_MASK, HOST_IS, IRQ_COMPLETION, IRQ_ERROR, MAX_PORTS, PX_CI, PX_IE, PX_IS,
    PX_SACT, PX_SERR, PX_TFD, SharedRegisters, TFD_ERR, read_port, write_port,
};

pub(crate) const IRQ_SNAPSHOT_CAPACITY: usize = 64;

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
    irq_delivery_enabled: AtomicBool,
    capture_active: AtomicBool,
    init_handler_taken: AtomicBool,
    init_handler_live: AtomicBool,
    io_handler_taken: AtomicBool,
    io_handler_live: AtomicBool,
}

impl HostShared {
    pub(crate) fn new(registers: SharedRegisters) -> Arc<Self> {
        Arc::new(Self {
            registers,
            ports: array::from_fn(|_| PortShared::new()),
            implemented_ports: AtomicU32::new(0),
            ready_ports: AtomicU32::new(0),
            irq_delivery_enabled: AtomicBool::new(false),
            capture_active: AtomicBool::new(false),
            init_handler_taken: AtomicBool::new(false),
            init_handler_live: AtomicBool::new(false),
            io_handler_taken: AtomicBool::new(false),
            io_handler_live: AtomicBool::new(false),
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
        self.irq_delivery_enabled.store(enabled, Ordering::Release);
    }

    pub(crate) fn irq_delivery_enabled(&self) -> bool {
        self.irq_delivery_enabled.load(Ordering::Acquire)
    }

    pub(crate) fn take_initial_handler(self: &Arc<Self>) -> Option<Box<dyn IrqHandler>> {
        self.init_handler_taken
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        self.init_handler_live.store(true, Ordering::Release);
        Some(Box::new(AhciIrqHandler::new(
            Arc::clone(self),
            IrqEndpointRole::Initialization,
        )))
    }

    pub(crate) fn initial_handler_live(&self) -> bool {
        self.init_handler_live.load(Ordering::Acquire)
    }

    pub(crate) fn take_io_handler(self: &Arc<Self>) -> Option<Box<dyn IrqHandler>> {
        if self.initial_handler_live() {
            return None;
        }
        self.io_handler_taken
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        self.io_handler_live.store(true, Ordering::Release);
        Some(Box::new(AhciIrqHandler::new(
            Arc::clone(self),
            IrqEndpointRole::NormalIo,
        )))
    }

    pub(crate) fn io_handler_live(&self) -> bool {
        self.io_handler_live.load(Ordering::Acquire)
    }

    pub(crate) fn try_claim_register_window(&self) -> Option<CaptureGuard<'_>> {
        CaptureGuard::try_acquire(&self.capture_active)
    }

    fn capture_irq(&self) -> IrqOutcome {
        if !self.irq_delivery_enabled() {
            return IrqOutcome::unhandled();
        }
        let Some(_capture) = CaptureGuard::try_acquire(&self.capture_active) else {
            // A second retained handler endpoint may race during route handoff.
            // Leave the level source asserted; the OS action will retry after
            // the unique destructive owner releases the register window.
            return IrqOutcome::unhandled();
        };
        let host_status = self.registers.read32(HOST_IS);
        if host_status == 0 {
            return IrqOutcome::unhandled();
        }

        let pending_ports = host_status & self.implemented_ports();
        let mut event = Event::none();
        for port in 0..MAX_PORTS {
            if pending_ports & (1 << port) == 0 {
                continue;
            }
            if self.capture_port_irq(port) {
                event.push_queue(port);
            }
        }

        // AHCI host status is a level-triggered latch. Every port status must
        // be acknowledged before the unmasked host value is cleared.
        self.registers.write32(HOST_IS, host_status);
        if event.is_empty() {
            IrqOutcome::handled_control()
        } else {
            IrqOutcome::handled(event)
        }
    }

    fn capture_port_irq(&self, port: usize) -> bool {
        let status = read_port(self.registers(), port, PX_IS);
        if status == 0 {
            return false;
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

        if !self.port(port).snapshots.push(snapshot) {
            self.port(port).overflow.store(true, Ordering::Release);
            // Overflow loses stable device facts, so freeze this port and let
            // the bounded worker enter controller recovery.
            write_port(self.registers(), port, PX_IE, 0);
        }
        true
    }

    pub(crate) fn mask_all_ports(&self) {
        for port in 0..MAX_PORTS {
            if self.implemented_ports() & (1 << port) != 0 {
                write_port(self.registers(), port, PX_IE, 0);
            }
        }
    }

    pub(crate) fn mask_port(&self, port: usize) {
        write_port(self.registers(), port, PX_IE, 0);
    }

    pub(crate) fn unmask_ready_ports(&self) {
        for port in 0..MAX_PORTS {
            if self.ready_ports() & (1 << port) != 0 {
                write_port(self.registers(), port, PX_IE, DEFAULT_PORT_IRQ_MASK);
            }
        }
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

impl IrqHandler for AhciIrqHandler {
    fn handle_irq(&mut self) -> IrqOutcome {
        self.shared.capture_irq()
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

// SAFETY: `HostShared::capture_active` serializes the retained initialization
// and normal handler endpoints into one effective producer. Controller runtime
// serialization provides one consumer for each port. Release publication of
// `head` happens after slot initialization; Acquire observation happens before
// the consumer read.
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

        assert!(outcome.is_handled());
        assert!(snapshot.has_error());
        assert!(snapshot.completes(0, generation));
        assert_eq!(snapshot.sata_error, 0x40);
    }

    #[test]
    fn irq_acknowledges_port_status_before_host_latch() {
        let registers = FakeRegisters::new(MMIO_REQUIRED_SIZE);
        let shared = HostShared::new(registers.shared());
        shared.publish_implemented_ports(1);
        shared.set_irq_delivery_enabled(true);
        registers.set(HOST_IS, 1);
        registers.set(port_offset(0, PX_IS), IRQ_D2H_REG_FIS);

        assert!(shared.capture_irq().is_handled());

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
        assert!(!contended.is_handled());
        assert!(!contended.is_deferred());
        assert!(registers.writes().is_empty());

        drop(capture);
        let retried = shared.capture_irq();
        assert!(retried.is_handled());
        assert!(!retried.is_deferred());
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
        assert!(outcome.is_handled());
        let event = outcome.acknowledged_event().unwrap();
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
        assert!(!shared.capture_irq().is_handled());

        shared.set_irq_delivery_enabled(true);
        assert!(shared.capture_irq().is_handled());
        let snapshot = shared.port(0).pop_snapshot().unwrap();
        assert!(snapshot.completes(0, generation));

        assert!(shared.port(0).clear_active_request(generation));
        let next_generation = shared.port(0).next_request_generation();
        assert!(shared.port(0).publish_active_request(next_generation));
        assert!(!snapshot.completes(0, next_generation));
    }
}
