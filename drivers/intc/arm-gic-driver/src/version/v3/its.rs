use core::hint::spin_loop;

use aarch64_cpu::asm::barrier;
use tock_registers::{interfaces::*, register_bitfields, register_structs, registers::*};

use crate::VirtAddr;

pub const GITS_TRANSLATER_OFFSET: u64 = 0x10040;
pub const ITS_COMMAND_SIZE: usize = core::mem::size_of::<ItsCommand>();

register_structs! {
    #[allow(non_snake_case)]
    pub ItsRegs {
        (0x0000 => pub CTLR: ReadWrite<u32, CTLR::Register>),
        (0x0004 => pub IIDR: ReadOnly<u32>),
        (0x0008 => pub TYPER: ReadOnly<u64, TYPER::Register>),
        (0x0010 => _rsv0),
        (0x0018 => pub MPIDR: ReadOnly<u64>),
        (0x0020 => _rsv1: [u32; 24]),
        (0x0080 => pub CBASER: ReadWrite<u64, CBASER::Register>),
        (0x0088 => pub CWRITER: ReadWrite<u64, CWRITER::Register>),
        (0x0090 => pub CREADR: ReadOnly<u64, CREADR::Register>),
        (0x0098 => _rsv2: [u32; 26]),
        (0x0100 => pub BASER: [ReadWrite<u64, BASER::Register>; 8]),
        (0x0140 => _rsv3: [u32; 16320]),
        (0x10040 => pub TRANSLATER: WriteOnly<u32>),
        (0x10044 => @END),
    }
}

register_bitfields! [
    u32,
    pub CTLR [
        Enabled OFFSET(0) NUMBITS(1) [],
        ImDe OFFSET(1) NUMBITS(1) [],
        ITSNumber OFFSET(4) NUMBITS(4) [],
        Quiescent OFFSET(31) NUMBITS(1) [],
    ],
];

register_bitfields! [
    u64,
    pub TYPER [
        PhysicalLPIs OFFSET(0) NUMBITS(1) [],
        VirtualLPIs OFFSET(1) NUMBITS(1) [],
        ITTEntrySize OFFSET(4) NUMBITS(4) [],
        IDbits OFFSET(8) NUMBITS(5) [],
        Devbits OFFSET(13) NUMBITS(5) [],
        PTA OFFSET(19) NUMBITS(1) [],
        HCC OFFSET(24) NUMBITS(8) [],
    ],
    pub CBASER [
        Size OFFSET(0) NUMBITS(8) [],
        Shareability OFFSET(10) NUMBITS(2) [
            NonShareable = 0,
            InnerShareable = 0b01,
            OuterShareable = 0b10,
        ],
        PhysicalAddress OFFSET(12) NUMBITS(40) [],
        OuterCache OFFSET(53) NUMBITS(3) [
            NonCacheable = 0b001,
            RaWaWb = 0b111,
        ],
        InnerCache OFFSET(59) NUMBITS(3) [
            NonCacheable = 0b001,
            RaWaWb = 0b111,
        ],
        Valid OFFSET(63) NUMBITS(1) [],
    ],
    pub CWRITER [
        Retry OFFSET(0) NUMBITS(1) [],
        Offset OFFSET(5) NUMBITS(15) [],
    ],
    pub CREADR [
        Stalled OFFSET(0) NUMBITS(1) [],
        Offset OFFSET(5) NUMBITS(15) [],
    ],
    pub BASER [
        Size OFFSET(0) NUMBITS(8) [],
        PageSize OFFSET(8) NUMBITS(2) [
            Size4K = 0,
            Size16K = 1,
            Size64K = 2,
        ],
        Shareability OFFSET(10) NUMBITS(2) [
            NonShareable = 0,
            InnerShareable = 0b01,
            OuterShareable = 0b10,
        ],
        PhysicalAddress OFFSET(12) NUMBITS(36) [],
        EntrySize OFFSET(48) NUMBITS(5) [],
        OuterCache OFFSET(53) NUMBITS(3) [
            NonCacheable = 0b001,
            RaWaWb = 0b111,
        ],
        Type OFFSET(56) NUMBITS(3) [
            None = 0,
            Device = 1,
            Vcpu = 2,
            Collection = 4,
        ],
        InnerCache OFFSET(59) NUMBITS(3) [
            NonCacheable = 0b001,
            RaWaWb = 0b111,
        ],
        Indirect OFFSET(62) NUMBITS(1) [],
        Valid OFFSET(63) NUMBITS(1) [],
    ],
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItsTableType {
    Device,
    Collection,
}

impl ItsTableType {
    const fn baser_value(self) -> u64 {
        match self {
            Self::Device => 1,
            Self::Collection => 4,
        }
    }
}

#[repr(C, align(32))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ItsCommand {
    raw: [u64; 4],
}

impl ItsCommand {
    pub const fn raw(self) -> [u64; 4] {
        self.raw
    }

    pub fn mapd(device_id: u32, itt_phys: u64, event_count: u32, valid: bool) -> Self {
        let mut cmd = Self::opcode(0x08);
        cmd.encode_device(device_id);
        cmd.encode_size(itt_size_field(event_count));
        cmd.encode_itt(itt_phys);
        cmd.encode_valid(valid);
        cmd.le()
    }

    pub fn mapc(collection: u16, target_address: u64, valid: bool) -> Self {
        let mut cmd = Self::opcode(0x09);
        cmd.encode_collection(collection);
        cmd.encode_target(target_address);
        cmd.encode_valid(valid);
        cmd.le()
    }

    pub fn mapti(device_id: u32, event_id: u32, physical_id: u32, collection: u16) -> Self {
        let mut cmd = Self::opcode(0x0a);
        cmd.encode_device(device_id);
        cmd.encode_event(event_id);
        cmd.encode_physical(physical_id);
        cmd.encode_collection(collection);
        cmd.le()
    }

    pub fn inv(device_id: u32, event_id: u32) -> Self {
        let mut cmd = Self::opcode(0x0c);
        cmd.encode_device(device_id);
        cmd.encode_event(event_id);
        cmd.le()
    }

    pub fn sync(target_address: u64) -> Self {
        let mut cmd = Self::opcode(0x05);
        cmd.encode_target(target_address);
        cmd.le()
    }

    const fn opcode(opcode: u8) -> Self {
        Self {
            raw: [opcode as u64, 0, 0, 0],
        }
    }

    fn encode_device(&mut self, device_id: u32) {
        self.raw[0] |= u64::from(device_id) << 32;
    }

    fn encode_event(&mut self, event_id: u32) {
        self.raw[1] |= u64::from(event_id);
    }

    fn encode_physical(&mut self, physical_id: u32) {
        self.raw[1] |= u64::from(physical_id) << 32;
    }

    fn encode_size(&mut self, size: u8) {
        self.raw[1] |= u64::from(size) & 0x1f;
    }

    fn encode_itt(&mut self, itt_phys: u64) {
        self.raw[2] |= ((itt_phys >> 8) & ((1 << 44) - 1)) << 8;
    }

    fn encode_target(&mut self, target_address: u64) {
        self.raw[2] |= ((target_address >> 16) & ((1 << 36) - 1)) << 16;
    }

    fn encode_collection(&mut self, collection: u16) {
        self.raw[2] |= u64::from(collection);
    }

    fn encode_valid(&mut self, valid: bool) {
        self.raw[2] |= u64::from(valid) << 63;
    }

    fn le(mut self) -> Self {
        for word in &mut self.raw {
            *word = word.to_le();
        }
        self
    }
}

pub fn itt_size_field(event_count: u32) -> u8 {
    let entries = event_count.next_power_of_two();
    let entries = if entries < 2 { 2 } else { entries };
    entries.ilog2().saturating_sub(1) as u8
}

pub struct Its {
    base: VirtAddr,
    phys_base: u64,
}

unsafe impl Send for Its {}

impl Its {
    /// # Safety
    ///
    /// `base` must be a valid mapping of a GICv3 ITS register frame, and
    /// `phys_base` must be the matching physical address used in MSI messages.
    pub const unsafe fn new(base: VirtAddr, phys_base: u64) -> Self {
        Self { base, phys_base }
    }

    pub const fn phys_base(&self) -> u64 {
        self.phys_base
    }

    pub const fn translater_address(&self) -> u64 {
        self.phys_base + GITS_TRANSLATER_OFFSET
    }

    pub fn typer(&self) -> u64 {
        self.regs().TYPER.get()
    }

    pub fn id_bits(&self) -> u8 {
        self.regs().TYPER.read(TYPER::IDbits) as u8 + 1
    }

    pub fn dev_bits(&self) -> u8 {
        self.regs().TYPER.read(TYPER::Devbits) as u8 + 1
    }

    pub fn itt_entry_size(&self) -> usize {
        self.regs().TYPER.read(TYPER::ITTEntrySize) as usize + 1
    }

    pub fn supports_physical_lpis(&self) -> bool {
        self.regs().TYPER.is_set(TYPER::PhysicalLPIs)
    }

    pub fn uses_physical_collection_target(&self) -> bool {
        self.regs().TYPER.is_set(TYPER::PTA)
    }

    pub fn baser_type(&self, index: usize) -> Option<ItsTableType> {
        match self.regs().BASER[index].read(BASER::Type) {
            1 => Some(ItsTableType::Device),
            4 => Some(ItsTableType::Collection),
            _ => None,
        }
    }

    pub fn baser_entry_size(&self, index: usize) -> usize {
        self.regs().BASER[index].read(BASER::EntrySize) as usize + 1
    }

    pub fn init_command_queue(&self, queue_phys: u64, queue_bytes: usize) {
        let pages = queue_bytes.div_ceil(4096).clamp(1, 256);
        self.regs().CWRITER.set(0);
        self.regs().CBASER.write(
            CBASER::Valid::SET
                + CBASER::Size.val((pages - 1) as u64)
                + CBASER::PhysicalAddress.val(queue_phys >> 12)
                + CBASER::Shareability::InnerShareable
                + CBASER::InnerCache::RaWaWb
                + CBASER::OuterCache::RaWaWb,
        );
        barrier::dsb(barrier::SY);
    }

    pub fn program_baser(&self, index: usize, value: u64) {
        self.regs().BASER[index].set(value);
        barrier::dsb(barrier::SY);
    }

    pub fn baser_value(
        table_type: ItsTableType,
        phys: u64,
        bytes: usize,
        entry_size: usize,
    ) -> u64 {
        let pages = bytes.div_ceil(4096).clamp(1, 256);
        (BASER::Valid::SET
            + BASER::Size.val((pages - 1) as u64)
            + BASER::PageSize::Size4K
            + BASER::PhysicalAddress.val(phys >> 12)
            + BASER::Shareability::InnerShareable
            + BASER::InnerCache::RaWaWb
            + BASER::OuterCache::RaWaWb
            + BASER::EntrySize.val(entry_size.saturating_sub(1) as u64))
        .value
            | (table_type.baser_value() << 56)
    }

    pub fn enable(&self) {
        self.regs().CTLR.modify(CTLR::Enabled::SET);
        barrier::isb(barrier::SY);
    }

    pub fn disable(&self) {
        self.regs().CTLR.modify(CTLR::Enabled::CLEAR);
        barrier::isb(barrier::SY);
        while !self.regs().CTLR.is_set(CTLR::Quiescent) {
            spin_loop();
        }
    }

    pub fn write_cwriter(&self, byte_offset: usize) {
        self.regs()
            .CWRITER
            .write(CWRITER::Offset.val((byte_offset >> 5) as u64));
        barrier::dsb(barrier::SY);
    }

    pub fn creadr_offset(&self) -> usize {
        (self.regs().CREADR.read(CREADR::Offset) as usize) << 5
    }

    fn regs(&self) -> &ItsRegs {
        unsafe { &*self.base.as_ptr() }
    }
}
