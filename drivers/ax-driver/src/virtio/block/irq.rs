//! Independent VirtIO interrupt-status ownership.
//!
//! The registered hard-IRQ action owns [`VirtioInterruptPort`]. It reads and
//! acknowledges only the transport's dedicated interrupt-status registers;
//! it never borrows the queue/configuration transport used by the maintenance
//! owner. The matching control endpoint validates source generations. VirtIO
//! MMIO and PCI do not expose a portable hard-IRQ-safe device-source mask, so
//! failed publication is reported as uncontained and the OS masks the action
//! or line.

#[cfg(test)]
use alloc::boxed::Box;
use alloc::sync::Arc;
#[cfg(test)]
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use rdif_block::BlkError;
#[cfg(test)]
use rdif_block::{
    BlockIrqSource, ContainmentCause, Event, IrqCapture, IrqControlError, IrqEndpoint,
    IrqSourceControl, MaskedSource,
};
#[cfg(test)]
use virtio_drivers::transport::InterruptStatus;

#[cfg(test)]
use super::VIRTIO_BLK_QUEUE_ID;

const MMIO_INTERRUPT_STATUS_OFFSET: usize = 0x60;
const MMIO_INTERRUPT_ACK_OFFSET: usize = 0x64;
const MMIO_INTERRUPT_REGISTERS_END: usize = MMIO_INTERRUPT_ACK_OFFSET + size_of::<u32>();
#[cfg(test)]
const VIRTIO_QUEUE_SOURCE_BITMAP: u64 = 1;

/// Destructive VirtIO interrupt-status capability separated from `Transport`.
pub struct VirtioInterruptPort {
    registers: VirtioInterruptRegisters,
}

impl VirtioInterruptPort {
    /// Builds a VirtIO MMIO interrupt port and retains its complete mapping.
    ///
    /// The mapping must describe the same controller from which the paired
    /// queue/config transport was created. Registration helpers construct both
    /// parts before erasing the platform transport type.
    pub fn from_mmio(mapping: mmio_api::Mmio) -> Result<Self, BlkError> {
        if mapping.size() < MMIO_INTERRUPT_REGISTERS_END {
            return Err(BlkError::Other(
                "virtio MMIO mapping does not contain interrupt registers",
            ));
        }
        Ok(Self {
            registers: VirtioInterruptRegisters::Mmio {
                mapping: Arc::new(mapping),
            },
        })
    }

    /// Builds a VirtIO PCI interrupt port and retains its ISR mapping.
    ///
    /// The mapping must come from the paired controller's vendor ISR
    /// capability; PCI discovery validates its BAR and bounds before calling
    /// this constructor.
    pub fn from_pci_isr(mapping: mmio_api::Mmio) -> Result<Self, BlkError> {
        if mapping.size() < size_of::<u8>() {
            return Err(BlkError::Other("virtio PCI ISR mapping is empty"));
        }
        Ok(Self {
            registers: VirtioInterruptRegisters::Pci {
                mapping: Arc::new(mapping),
            },
        })
    }

    #[cfg(test)]
    fn for_test(status: Arc<AtomicU8>) -> Self {
        Self {
            registers: VirtioInterruptRegisters::Test { status },
        }
    }

    /// Reads and acknowledges one raw transport ISR snapshot.
    ///
    /// The v0.13 evidence endpoint consumes this port and stores the complete
    /// snapshot in its private ledger before returning an opaque identity.
    pub(super) fn capture_raw_status(&mut self) -> u32 {
        self.registers.capture_status().raw
    }

    /// Clones only the retained MMIO mapping needed to build a queue-notify
    /// port. PCI transports use a distinct vendor notify capability instead.
    pub(super) fn mmio_mapping(&self) -> Option<Arc<mmio_api::Mmio>> {
        match &self.registers {
            VirtioInterruptRegisters::Mmio { mapping } => Some(Arc::clone(mapping)),
            VirtioInterruptRegisters::Pci { .. } => None,
            #[cfg(test)]
            VirtioInterruptRegisters::Test { .. } => None,
        }
    }
}

#[derive(Clone)]
enum VirtioInterruptRegisters {
    Mmio {
        mapping: Arc<mmio_api::Mmio>,
    },
    Pci {
        mapping: Arc<mmio_api::Mmio>,
    },
    #[cfg(test)]
    Test {
        status: Arc<AtomicU8>,
    },
}

impl VirtioInterruptRegisters {
    fn capture_status(&mut self) -> CapturedInterruptStatus {
        let raw = match self {
            Self::Mmio { mapping } => {
                let raw = mapping.read::<u32>(MMIO_INTERRUPT_STATUS_OFFSET);
                if raw != 0 {
                    mapping.write(MMIO_INTERRUPT_ACK_OFFSET, raw);
                }
                raw
            }
            // The VirtIO PCI specification defines this read itself as the
            // destructive acknowledgement.
            Self::Pci { mapping } => u32::from(mapping.read::<u8>(0)),
            #[cfg(test)]
            Self::Test { status } => u32::from(status.swap(0, Ordering::AcqRel)),
        };
        CapturedInterruptStatus {
            raw,
            #[cfg(test)]
            known: InterruptStatus::from_bits_truncate(raw),
        }
    }
}

struct CapturedInterruptStatus {
    raw: u32,
    #[cfg(test)]
    known: InterruptStatus,
}

/// One-controller factory for initialization and normal-I/O IRQ endpoints.
#[cfg(test)]
pub(super) struct VirtioIrqOwnership {
    registers: VirtioInterruptRegisters,
    state: Arc<VirtioIrqState>,
    initialization_taken: bool,
    normal_io_taken: bool,
}

/// Keeps a controller register mapping alive across split queue and IRQ
/// objects. MMIO transports retain raw register pointers, while PCI uses this
/// lease only for its ISR capability. The lease exposes no register operations:
/// destructive status ownership remains exclusively in the IRQ endpoint.
#[derive(Clone)]
#[cfg(test)]
pub(super) struct VirtioRegisterMappingLease {
    _registers: VirtioInterruptRegisters,
}

#[cfg(test)]
impl VirtioIrqOwnership {
    pub(super) fn new(port: VirtioInterruptPort) -> Self {
        Self {
            registers: port.registers,
            state: Arc::new(VirtioIrqState::new()),
            initialization_taken: false,
            normal_io_taken: false,
        }
    }

    pub(super) fn enable(&self) {
        self.state.enable();
    }

    pub(super) fn disable(&self) {
        self.state.enabled.store(false, Ordering::Release);
    }

    pub(super) fn is_enabled(&self) -> bool {
        self.state.enabled.load(Ordering::Acquire)
    }

    pub(super) fn register_mapping_lease(&self) -> VirtioRegisterMappingLease {
        VirtioRegisterMappingLease {
            _registers: self.registers.clone(),
        }
    }

    pub(super) fn initialization_is_live(&self) -> bool {
        self.state.initialization_live.load(Ordering::Acquire)
    }

    pub(super) fn normal_io_is_live(&self) -> bool {
        self.state.normal_io_live.load(Ordering::Acquire)
    }

    pub(super) fn take_initialization_source(&mut self) -> Option<BlockIrqSource> {
        if self.initialization_taken {
            return None;
        }
        self.initialization_taken = true;
        self.state
            .initialization_live
            .store(true, Ordering::Release);
        Some(self.new_source(IrqEndpointRole::Initialization))
    }

    pub(super) fn take_normal_io_source(&mut self) -> Option<BlockIrqSource> {
        if self.normal_io_taken || self.initialization_is_live() {
            return None;
        }
        self.normal_io_taken = true;
        self.state.normal_io_live.store(true, Ordering::Release);
        Some(self.new_source(IrqEndpointRole::NormalIo))
    }

    fn new_source(&self, role: IrqEndpointRole) -> BlockIrqSource {
        BlockIrqSource::new(
            Box::new(VirtioBlkIrqEndpoint {
                registers: self.registers.clone(),
                state: Arc::clone(&self.state),
                role,
            }),
            Box::new(VirtioBlkIrqControl {
                state: Arc::clone(&self.state),
            }),
        )
    }
}

#[cfg(test)]
struct VirtioIrqState {
    enabled: AtomicBool,
    generation: AtomicU64,
    initialization_live: AtomicBool,
    normal_io_live: AtomicBool,
}

#[cfg(test)]
impl VirtioIrqState {
    const fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            generation: AtomicU64::new(1),
            initialization_live: AtomicBool::new(false),
            normal_io_live: AtomicBool::new(false),
        }
    }

    fn enable(&self) {
        let previous = self.enabled.swap(true, Ordering::AcqRel);
        if previous {
            return;
        }
        let next = self
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        if next == 0 {
            self.generation.store(1, Ordering::Release);
        }
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy)]
#[cfg(test)]
enum IrqEndpointRole {
    Initialization,
    NormalIo,
}

#[cfg(test)]
struct VirtioBlkIrqEndpoint {
    registers: VirtioInterruptRegisters,
    state: Arc<VirtioIrqState>,
    role: IrqEndpointRole,
}

#[cfg(test)]
impl IrqEndpoint for VirtioBlkIrqEndpoint {
    type Event = Event;
    type Fault = BlkError;

    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault> {
        if !self.state.enabled.load(Ordering::Acquire) {
            return IrqCapture::Unhandled;
        }
        let status = self.registers.capture_status();
        if status.raw == 0 {
            return IrqCapture::Unhandled;
        }
        IrqCapture::Captured {
            // A non-zero raw value was destructively acknowledged even when a
            // newer device used only reserved bits. Preserve IRQ ownership as
            // a control event instead of misreporting the shared line as
            // unhandled; only known queue bits activate completion service.
            event: virtio_blk_event_from_irq_status(status.known),
            masked: None,
        }
    }

    fn contain(&mut self, _cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
        // Neither VirtIO MMIO nor the portable PCI ISR capability contains a
        // device-side interrupt mask. Pretending that a software flag masks
        // hardware would permit an IRQ storm, so the OS must mask the action
        // or parent line and enter controller recovery.
        Err(BlkError::Other(
            "virtio interrupt source cannot be contained from hard IRQ",
        ))
    }
}

#[cfg(test)]
impl Drop for VirtioBlkIrqEndpoint {
    fn drop(&mut self) {
        match self.role {
            IrqEndpointRole::Initialization => self
                .state
                .initialization_live
                .store(false, Ordering::Release),
            IrqEndpointRole::NormalIo => self.state.normal_io_live.store(false, Ordering::Release),
        }
    }
}

#[cfg(test)]
struct VirtioBlkIrqControl {
    state: Arc<VirtioIrqState>,
}

#[cfg(test)]
impl IrqSourceControl for VirtioBlkIrqControl {
    type Error = IrqControlError;

    fn rearm(&mut self, source: MaskedSource) -> Result<(), Self::Error> {
        let expected = self.state.generation();
        let actual = source.generation().get();
        if actual != expected {
            return Err(IrqControlError::StaleGeneration { expected, actual });
        }
        Err(IrqControlError::SourceNotMasked {
            bitmap: source.bitmap().get(),
        })
    }
}

#[cfg(test)]
pub(super) fn virtio_blk_event_from_irq_status(status: InterruptStatus) -> Event {
    if !status.contains(InterruptStatus::QUEUE_INTERRUPT) {
        return Event::none();
    }
    Event::from_queue_bits(VIRTIO_QUEUE_SOURCE_BITMAP << VIRTIO_BLK_QUEUE_ID)
}

#[cfg(test)]
pub(super) fn test_interrupt_port(status: Arc<AtomicU8>) -> VirtioInterruptPort {
    VirtioInterruptPort::for_test(status)
}

#[cfg(test)]
pub(super) fn test_register_mapping_lease() -> VirtioRegisterMappingLease {
    VirtioRegisterMappingLease {
        _registers: VirtioInterruptRegisters::Test {
            status: Arc::new(AtomicU8::new(0)),
        },
    }
}
