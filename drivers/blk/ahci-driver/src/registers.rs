use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};

use mmio_api::{Mmio, MmioRaw};
use rdif_block::{
    ControlEvent, GroupIrqEvent, GroupIrqSink, IrqDisposition, IrqQueueMask, SharedHardIrqHandler,
};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields, register_structs,
    registers::ReadWrite,
};

use crate::AhciError;

pub(super) const IRQ_SOURCE_ID: usize = 0;
pub(super) const QUEUE_ID: usize = 0;
pub(super) const CONTROL_EVENT_IRQ: u64 = 1 << 63;

const PORT_BASE: usize = 0x100;
const PORT_STRIDE: usize = 0x80;
const SATA_SIGNATURE: u32 = 0x0000_0101;

pub(super) const PORT_IRQ_FATAL: u32 =
    (1 << 30) | (1 << 29) | (1 << 28) | (1 << 27) | (1 << 26) | (1 << 24) | (1 << 4);
pub(super) const PORT_IRQ_COMPLETIONS: u32 = (1 << 5) | (1 << 3) | (1 << 2) | (1 << 1) | (1 << 0);
pub(super) const PORT_IRQ_LINK: u32 = (1 << 23) | (1 << 22) | (1 << 6);
const PORT_IRQ_ENABLE: u32 = PORT_IRQ_FATAL | PORT_IRQ_COMPLETIONS | PORT_IRQ_LINK;

register_bitfields![u32,
    Capabilities [
        NP OFFSET(0) NUMBITS(5) [],
        NCS OFFSET(8) NUMBITS(5) [],
        SSS OFFSET(27) NUMBITS(1) [],
        SNCQ OFFSET(30) NUMBITS(1) [],
        S64A OFFSET(31) NUMBITS(1) []
    ],
    GlobalHostControl [
        HR OFFSET(0) NUMBITS(1) [],
        IE OFFSET(1) NUMBITS(1) [],
        AE OFFSET(31) NUMBITS(1) []
    ],
    PortCommand [
        ST OFFSET(0) NUMBITS(1) [],
        SUD OFFSET(1) NUMBITS(1) [],
        FRE OFFSET(4) NUMBITS(1) [],
        FR OFFSET(14) NUMBITS(1) [],
        CR OFFSET(15) NUMBITS(1) [],
        ICC OFFSET(28) NUMBITS(4) [
            Active = 1
        ]
    ],
    TaskFileData [
        ERR OFFSET(0) NUMBITS(1) [],
        DRQ OFFSET(3) NUMBITS(1) [],
        BSY OFFSET(7) NUMBITS(1) []
    ],
    SataStatus [
        DET OFFSET(0) NUMBITS(4) [
            Present = 3
        ],
        IPM OFFSET(8) NUMBITS(4) [
            Active = 1
        ]
    ]
];

register_structs! {
    HbaRegisterBlock {
        (0x00 => capabilities: ReadWrite<u32, Capabilities::Register>),
        (0x04 => control: ReadWrite<u32, GlobalHostControl::Register>),
        (0x08 => interrupt_status: ReadWrite<u32>),
        (0x0c => ports_implemented: ReadWrite<u32>),
        (0x10 => version: ReadWrite<u32>),
        (0x14 => _reserved0),
        (0x24 => @END),
    }
}

register_structs! {
    PortRegisterBlock {
        (0x00 => command_list_base_low: ReadWrite<u32>),
        (0x04 => command_list_base_high: ReadWrite<u32>),
        (0x08 => fis_base_low: ReadWrite<u32>),
        (0x0c => fis_base_high: ReadWrite<u32>),
        (0x10 => interrupt_status: ReadWrite<u32>),
        (0x14 => interrupt_enable: ReadWrite<u32>),
        (0x18 => command: ReadWrite<u32, PortCommand::Register>),
        (0x1c => _reserved0),
        (0x20 => task_file_data: ReadWrite<u32, TaskFileData::Register>),
        (0x24 => signature: ReadWrite<u32>),
        (0x28 => sata_status: ReadWrite<u32, SataStatus::Register>),
        (0x2c => sata_control: ReadWrite<u32>),
        (0x30 => sata_error: ReadWrite<u32>),
        (0x34 => sata_active: ReadWrite<u32>),
        (0x38 => command_issue: ReadWrite<u32>),
        (0x3c => sata_notification: ReadWrite<u32>),
        (0x40 => fis_switch_control: ReadWrite<u32>),
        (0x44 => _reserved1),
        (0x80 => @END),
    }
}

struct RegisterRegion {
    raw: MmioRaw,
    _mapping: MappingOwner,
}

enum MappingOwner {
    Owned {
        _mapping: Arc<Mmio>,
    },
    #[cfg(test)]
    Borrowed,
}

// SAFETY: `raw` is retained only while `_mapping` owns the MMIO mapping.
// Device register synchronization is split by contract: the group controller
// owns HBA lifecycle registers, each member task owns its port command state,
// and the IRQ endpoint only reads/acks status after masking the port source.
unsafe impl Send for RegisterRegion {}
unsafe impl Sync for RegisterRegion {}

#[derive(Clone)]
pub(super) struct HbaRegisters {
    region: Arc<RegisterRegion>,
}

impl HbaRegisters {
    pub(super) fn new(mmio: Mmio) -> Result<Self, AhciError> {
        if mmio.size() < PORT_BASE {
            return Err(AhciError::MmioTooSmall {
                actual: mmio.size(),
                required: PORT_BASE,
            });
        }
        let mapping = Arc::new(mmio);
        let raw = MmioRaw::clone(&mapping);
        Ok(Self {
            region: Arc::new(RegisterRegion {
                raw,
                _mapping: MappingOwner::Owned { _mapping: mapping },
            }),
        })
    }

    pub(super) fn capabilities(&self) -> u32 {
        self.registers().capabilities.get()
    }

    pub(super) fn dma_mask(&self) -> u64 {
        if self.registers().capabilities.is_set(Capabilities::S64A) {
            u64::MAX
        } else {
            u64::from(u32::MAX)
        }
    }

    pub(super) fn command_slots(&self) -> usize {
        self.registers().capabilities.read(Capabilities::NCS) as usize + 1
    }

    pub(super) fn supports_ncq(&self) -> bool {
        self.registers().capabilities.is_set(Capabilities::SNCQ)
    }

    pub(super) fn supports_staggered_spin_up(&self) -> bool {
        self.registers().capabilities.is_set(Capabilities::SSS)
    }

    pub(super) fn initialize_staggered_spin_up_capability(&self) -> u32 {
        self.registers().capabilities.modify(Capabilities::SSS::SET);
        self.capabilities()
    }

    pub(super) fn begin_reset(&self) {
        self.registers()
            .control
            .modify(GlobalHostControl::AE::SET + GlobalHostControl::HR::SET);
    }

    pub(super) fn reset_complete(&self) -> bool {
        !self.registers().control.is_set(GlobalHostControl::HR)
    }

    pub(super) fn enable_ahci(&self) {
        self.registers()
            .control
            .modify(GlobalHostControl::AE::SET + GlobalHostControl::IE::CLEAR);
    }

    pub(super) fn set_interrupts_enabled(&self, enabled: bool) {
        let interrupts = if enabled {
            GlobalHostControl::IE::SET
        } else {
            GlobalHostControl::IE::CLEAR
        };
        self.registers()
            .control
            .modify(GlobalHostControl::AE::SET + interrupts);
    }

    pub(super) fn interrupt_status(&self) -> u32 {
        self.registers().interrupt_status.get()
    }

    pub(super) fn acknowledge_interrupts(&self, ports: u32) {
        self.registers().interrupt_status.set(ports);
    }

    pub(super) fn implemented_ports(&self) -> u32 {
        self.registers().ports_implemented.get()
    }

    pub(super) fn initialize_port_map(&self, fallback: u32) -> u32 {
        let implemented = self.implemented_ports();
        if implemented != 0 || fallback == 0 {
            return implemented;
        }
        self.registers().ports_implemented.set(fallback);
        self.implemented_ports()
    }

    pub(super) fn max_ports(&self) -> u8 {
        (self.registers().capabilities.read(Capabilities::NP) + 1) as u8
    }

    pub(super) fn port(&self, index: u8) -> Result<PortRegisters, AhciError> {
        let end = PORT_BASE
            .checked_add(usize::from(index) * PORT_STRIDE)
            .and_then(|start| start.checked_add(PORT_STRIDE))
            .ok_or(AhciError::InvalidPort(index))?;
        if index >= self.max_ports() || end > self.region.raw.size() {
            return Err(AhciError::InvalidPort(index));
        }
        Ok(PortRegisters {
            hba: self.clone(),
            index,
        })
    }

    fn registers(&self) -> &HbaRegisterBlock {
        // SAFETY: `new` validates the fixed HBA header and `_mapping` keeps the
        // naturally aligned register aperture live for this reference.
        unsafe { &*self.region.raw.as_ptr().cast::<HbaRegisterBlock>() }
    }

    #[cfg(test)]
    fn from_words(words: &mut [u32]) -> Self {
        let raw = unsafe {
            MmioRaw::new(
                0usize.into(),
                core::ptr::NonNull::new(words.as_mut_ptr().cast())
                    .expect("test register storage is non-null"),
                core::mem::size_of_val(words),
            )
        };
        Self {
            region: Arc::new(RegisterRegion {
                raw,
                _mapping: MappingOwner::Borrowed,
            }),
        }
    }
}

#[derive(Clone)]
pub(super) struct PortRegisters {
    hba: HbaRegisters,
    index: u8,
}

impl PortRegisters {
    pub(super) const fn index(&self) -> u8 {
        self.index
    }

    pub(super) fn task_file_status(&self) -> u32 {
        self.registers().task_file_data.get()
    }

    pub(super) fn sata_error(&self) -> u32 {
        self.registers().sata_error.get()
    }

    pub(super) fn command_issue(&self) -> u32 {
        self.registers().command_issue.get()
    }

    pub(super) fn sata_active(&self) -> u32 {
        self.registers().sata_active.get()
    }

    pub(super) fn signature(&self) -> u32 {
        self.registers().signature.get()
    }

    pub(super) fn is_sata_or_unknown(&self) -> bool {
        matches!(self.signature(), 0 | SATA_SIGNATURE)
    }

    pub(super) fn link_present(&self) -> bool {
        self.registers()
            .sata_status
            .matches_all(SataStatus::DET::Present + SataStatus::IPM::Active)
    }

    pub(super) fn task_file_ready(&self) -> bool {
        !self.registers().task_file_data.is_set(TaskFileData::BSY)
            && !self.registers().task_file_data.is_set(TaskFileData::DRQ)
    }

    pub(super) fn task_file_error(&self) -> bool {
        self.registers().task_file_data.is_set(TaskFileData::ERR)
    }

    pub(super) fn stop_command_engine(&self) {
        self.registers().command.modify(PortCommand::ST::CLEAR);
    }

    pub(super) fn command_engine_stopped(&self) -> bool {
        !self.registers().command.is_set(PortCommand::CR)
    }

    pub(super) fn stop_fis_receive(&self) {
        self.registers().command.modify(PortCommand::FRE::CLEAR);
    }

    pub(super) fn fis_receive_stopped(&self) -> bool {
        !self.registers().command.is_set(PortCommand::FR)
    }

    pub(super) fn engine_stopped(&self) -> bool {
        self.command_engine_stopped() && self.fis_receive_stopped()
    }

    pub(super) fn program_dma_bases(&self, command_list: u64, received_fis: u64) {
        self.registers()
            .command_list_base_low
            .set(command_list as u32);
        self.registers()
            .command_list_base_high
            .set((command_list >> 32) as u32);
        self.registers().fis_base_low.set(received_fis as u32);
        self.registers()
            .fis_base_high
            .set((received_fis >> 32) as u32);
    }

    pub(super) fn power_up(&self, staggered_spin_up: bool) {
        self.registers().command.modify(PortCommand::ICC::Active);
        if staggered_spin_up {
            self.registers().command.modify(PortCommand::SUD::SET);
        }
    }

    pub(super) fn start_fis_receive(&self) {
        self.registers().command.modify(PortCommand::FRE::SET);
    }

    pub(super) fn start_command_engine(&self) {
        self.registers().command.modify(PortCommand::ST::SET);
    }

    pub(super) fn clear_stale_status(&self) {
        self.registers().sata_error.set(u32::MAX);
        self.registers().interrupt_status.set(u32::MAX);
        self.hba.acknowledge_interrupts(1_u32 << self.index);
    }

    pub(super) fn set_interrupts_enabled(&self, enabled: bool) {
        self.registers()
            .interrupt_enable
            .set(if enabled { PORT_IRQ_ENABLE } else { 0 });
    }

    pub(super) fn active_slots(&self) -> u32 {
        self.command_issue() | self.sata_active()
    }

    pub(super) fn issue_commands(&self, slots: u32, queued_slots: u32) {
        if queued_slots != 0 {
            self.registers()
                .sata_active
                .set(self.sata_active() | queued_slots);
        }
        self.registers()
            .command_issue
            .set(self.command_issue() | slots);
    }

    fn pending_interrupts(&self) -> u32 {
        self.registers().interrupt_status.get()
    }

    fn acknowledge_port_interrupts(&self, status: u32) {
        self.registers().interrupt_status.set(status);
    }

    fn registers(&self) -> &PortRegisterBlock {
        let offset = PORT_BASE + usize::from(self.index) * PORT_STRIDE;
        // SAFETY: `HbaRegisters::port` validates this complete port aperture,
        // and the mapping owner outlives every cloned port capability.
        unsafe {
            &*self
                .hba
                .region
                .raw
                .as_ptr()
                .add(offset)
                .cast::<PortRegisterBlock>()
        }
    }
}

pub(super) struct PortIrqRoute {
    member_id: usize,
    port: PortRegisters,
    status_latch: Arc<AtomicU32>,
}

impl PortIrqRoute {
    pub(super) fn new(member_id: usize, port: PortRegisters, status_latch: Arc<AtomicU32>) -> Self {
        Self {
            member_id,
            port,
            status_latch,
        }
    }
}

pub(super) struct AhciHostIrq {
    hba: HbaRegisters,
    routes: Vec<PortIrqRoute>,
}

impl AhciHostIrq {
    pub(super) fn new(hba: HbaRegisters, routes: Vec<PortIrqRoute>) -> Self {
        Self { hba, routes }
    }
}

impl SharedHardIrqHandler for AhciHostIrq {
    fn ack(&mut self, sink: &mut dyn GroupIrqSink) -> IrqDisposition {
        let asserted = self.hba.interrupt_status();
        if asserted == 0 {
            return IrqDisposition::Spurious;
        }

        for route in &self.routes {
            let port_bit = 1_u32 << route.port.index();
            if asserted & port_bit == 0 {
                continue;
            }
            let status = route.port.pending_interrupts();
            let relevant = status & PORT_IRQ_ENABLE;
            if relevant != 0 {
                route.port.set_interrupts_enabled(false);
            }
            route.port.acknowledge_port_interrupts(status);
            if relevant == 0 {
                continue;
            }
            route.status_latch.fetch_or(relevant, Ordering::Release);
            sink.publish(GroupIrqEvent::member(
                route.member_id,
                IrqDisposition::MaskedNeedsRearm,
                IrqQueueMask::from_queue(QUEUE_ID),
                ControlEvent::new(IRQ_SOURCE_ID, CONTROL_EVENT_IRQ | u64::from(relevant)),
            ));
        }

        // HBA IS is W1C. Confirm the exact snapshot once after every routed
        // port has acknowledged its PxIS so no asserted port is lost.
        self.hba.acknowledge_interrupts(asserted);
        IrqDisposition::Cleared
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    const TEST_WORDS: usize = (PORT_BASE + 2 * PORT_STRIDE) / 4;

    #[derive(Default)]
    struct Events(Vec<GroupIrqEvent>);

    impl GroupIrqSink for Events {
        fn publish(&mut self, event: GroupIrqEvent) {
            self.0.push(event);
        }
    }

    #[test]
    fn register_layout_matches_ahci_offsets() {
        assert_eq!(core::mem::offset_of!(HbaRegisterBlock, capabilities), 0x00);
        assert_eq!(core::mem::offset_of!(HbaRegisterBlock, control), 0x04);
        assert_eq!(
            core::mem::offset_of!(HbaRegisterBlock, interrupt_status),
            0x08
        );
        assert_eq!(
            core::mem::offset_of!(PortRegisterBlock, interrupt_status),
            0x10
        );
        assert_eq!(core::mem::offset_of!(PortRegisterBlock, sata_active), 0x34);
        assert_eq!(
            core::mem::offset_of!(PortRegisterBlock, command_issue),
            0x38
        );
        assert_eq!(core::mem::size_of::<PortRegisterBlock>(), PORT_STRIDE);
    }

    #[test]
    fn one_irq_fans_out_two_ports_and_acknowledges_the_initial_global_status() {
        let mut words = vec![0_u32; TEST_WORDS];
        let hba = HbaRegisters::from_words(&mut words);
        words[0] = (1 << 30) | (31 << 8) | 1;
        words[2] = 0b111;
        let port0 = hba.port(0).expect("port zero");
        let port1 = hba.port(1).expect("port one");
        words[(PORT_BASE + 0x10) / 4] = 1;
        words[(PORT_BASE + PORT_STRIDE + 0x10) / 4] = 1 << 3;
        let first = Arc::new(AtomicU32::new(0));
        let second = Arc::new(AtomicU32::new(0));
        let mut handler = AhciHostIrq::new(
            hba,
            vec![
                PortIrqRoute::new(0, port0, Arc::clone(&first)),
                PortIrqRoute::new(1, port1, Arc::clone(&second)),
            ],
        );
        let mut events = Events::default();

        assert_eq!(handler.ack(&mut events), IrqDisposition::Cleared);
        assert_eq!(events.0.len(), 2);
        assert_eq!(events.0[0].target(), rdif_block::GroupIrqTarget::Member(0));
        assert_eq!(events.0[1].target(), rdif_block::GroupIrqTarget::Member(1));
        assert_eq!(first.load(Ordering::Acquire), 1);
        assert_eq!(second.load(Ordering::Acquire), 1 << 3);
        assert_eq!(
            words[2], 0b111,
            "the final HBA IS write must acknowledge routed and unrouted bits from one snapshot"
        );
    }

    #[test]
    fn asserted_port_without_relevant_status_is_handled_without_masking() {
        let mut words = vec![0_u32; TEST_WORDS];
        let hba = HbaRegisters::from_words(&mut words);
        words[0] = (1 << 30) | (31 << 8);
        words[2] = 1;
        let port = hba.port(0).expect("port zero");
        words[(PORT_BASE + 0x14) / 4] = PORT_IRQ_ENABLE;
        let status = Arc::new(AtomicU32::new(0));
        let mut handler =
            AhciHostIrq::new(hba, vec![PortIrqRoute::new(0, port, Arc::clone(&status))]);
        let mut events = Events::default();

        assert_eq!(handler.ack(&mut events), IrqDisposition::Cleared);
        assert!(events.0.is_empty());
        assert_eq!(status.load(Ordering::Acquire), 0);
        assert_eq!(
            words[(PORT_BASE + 0x14) / 4],
            PORT_IRQ_ENABLE,
            "an acknowledged global source without queue status must remain armed"
        );
    }

    #[test]
    fn platform_quirks_only_repair_requested_capabilities() {
        let mut words = vec![0_u32; TEST_WORDS];
        let hba = HbaRegisters::from_words(&mut words);

        assert_eq!(hba.initialize_port_map(1), 1);
        assert_eq!(
            hba.initialize_staggered_spin_up_capability() & (1 << 27),
            1 << 27
        );
    }
}
