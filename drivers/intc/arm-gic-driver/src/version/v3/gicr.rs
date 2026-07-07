//! GIC Redistributor (GICR) implementation for GICv3
//!
//! This module provides register definitions and functionality for the GIC Redistributor,
//! which is responsible for handling SGIs, PPIs, and LPIs for individual CPU cores.
//!
//! The Redistributor consists of two main register frames:
//! - RD_base: Controls LPI functionality and overall Redistributor behavior
//! - SGI_base: Controls SGIs and PPIs
//!
//! In GICv4, there are additional frames for virtual LPI support.

use core::{hint::spin_loop, ops::Index, ptr::NonNull};

use tock_registers::{interfaces::*, register_bitfields, register_structs, registers::*};

use crate::{IntId, define::Trigger, v3::Affinity};

pub type RDv3Slice = RedistributorSlice<RedistributorV3>;
#[allow(unused)]
pub type RDv4Slice = RedistributorSlice<RedistributorV4>;

pub trait RedistributorItem {
    fn lpi_ref(&self) -> &LPI;
}

pub(crate) struct RedistributorV3 {
    pub lpi: LPI,
    pub sgi: SGI,
}

#[allow(unused)]
pub(crate) struct RedistributorV4 {
    pub lpi: LPI,
    pub sgi: SGI,
    pub _vlpi: LPI,
    pub _vsgi: SGI,
}
impl RedistributorItem for RedistributorV3 {
    fn lpi_ref(&self) -> &LPI {
        &self.lpi
    }
}
impl RedistributorItem for RedistributorV4 {
    fn lpi_ref(&self) -> &LPI {
        &self.lpi
    }
}
pub struct RedistributorSlice<T: RedistributorItem> {
    ptr: NonNull<T>,
}

impl<T: RedistributorItem> RedistributorSlice<T> {
    pub fn new(ptr: NonNull<u8>) -> Self {
        Self { ptr: ptr.cast() }
    }

    pub fn iter(&self) -> RedistributorIter<T> {
        RedistributorIter::new(self.ptr)
    }
}

pub struct RedistributorIter<T: RedistributorItem> {
    ptr: NonNull<T>,
    is_last: bool,
}

impl<T: RedistributorItem> RedistributorIter<T> {
    pub fn new(p: NonNull<T>) -> Self {
        Self {
            ptr: p,
            is_last: false,
        }
    }
}

impl<T: RedistributorItem> Iterator for RedistributorIter<T> {
    type Item = NonNull<T>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.is_last {
            return None;
        }
        unsafe {
            let ptr = self.ptr;
            let rd = ptr.as_ref();
            let lpi = rd.lpi_ref();
            if lpi.TYPER.read(TYPER::Last) > 0 {
                self.is_last = true;
            }
            self.ptr = self.ptr.add(1);
            Some(ptr)
        }
    }
}

impl<T: RedistributorItem> Index<Affinity> for RedistributorSlice<T> {
    type Output = T;

    fn index(&self, index: Affinity) -> &Self::Output {
        let affinity = index.affinity();
        for rd in self.iter() {
            let affi = unsafe { rd.as_ref() }.lpi_ref().TYPER.read(TYPER::Affinity) as u32;
            if affi == affinity {
                return unsafe { rd.as_ref() };
            }
        }
        unreachable!()
    }
}

register_structs! {
    /// GIC Redistributor LPI registers.
    #[allow(non_snake_case)]
    pub LPI {
        (0x0000 => pub CTLR: ReadWrite<u32, RCtrl::Register>),
        (0x0004 => pub IIDR: ReadOnly<u32>),
        (0x0008 => pub TYPER: ReadOnly<u64, TYPER::Register>),
        (0x0010 => pub STATUSR: ReadWrite<u32>),
        (0x0014 => pub WAKER: ReadWrite<u32, WAKER::Register>),
        (0x0018 => pub MPAMIDR: ReadOnly<u32>),
        (0x001C => pub PARTIDR: ReadWrite<u32>),
        (0x0020 => _rsv0),
        (0x0040 => pub SETLPIR: WriteOnly<u64>),
        (0x0048 => pub CLRLPIR: WriteOnly<u64>),
        (0x0050 => _rsv1),
        (0x0070 => pub PROPBASER: ReadWrite<u64, PROPBASER::Register>),
        (0x0078 => pub PENDBASER: ReadWrite<u64, PENDBASER::Register>),
        (0x0080 => _rsv2),
        (0x00A0 => pub INVLPIR: WriteOnly<u64>),
        (0x00A8 => _rsv3),
        (0x00B0 => pub INVALLR: WriteOnly<u64>),
        (0x00B8 => _rsv4),
        (0x00C0 => pub SYNCR: ReadOnly<u32>),
        (0x00C4 => _rsv5),
        (0x0fe8 => pub PIDR2 : ReadOnly<u32, PIDR2::Register>),
        (0x0fec => _rsv6),
        (0x10000 => @END),
    }
}
register_bitfields! [
    u32,
    RCtrl [
        EnableLPIs OFFSET(0) NUMBITS(1) [],
        CES OFFSET(1) NUMBITS(1) [],
        IR  OFFSET(2) NUMBITS(1) [],
        RWP OFFSET(3) NUMBITS(1) [],
        DPG0 OFFSET(24) NUMBITS(1) [],
        DPG1NS OFFSET(25) NUMBITS(1) [],
        DPG1S OFFSET(26) NUMBITS(1) [],
        UWP OFFSET(31) NUMBITS(1) [],
    ],
    /// Peripheral ID2 Register
    PIDR2 [
        /// Architecture revision
        ArchRev OFFSET(4) NUMBITS(4) [],
    ],
];

register_bitfields! [
    u64,
    /// Redistributor Properties Base Address Register
    PROPBASER [
        IDbits OFFSET(0) NUMBITS(5) [],
        InnerCache OFFSET(7) NUMBITS(3) [
            NonCacheable = 0b001,
            WaWb = 0b111,
        ],
        Type OFFSET(10) NUMBITS(2) [],
        OuterCache OFFSET(56) NUMBITS(3) [
            NonCacheable = 0b001,
            WaWb = 0b111,
        ],
        PhysicalAddress OFFSET(12) NUMBITS(40) [],
    ],
    /// Redistributor LPI Pending Table Base Address Register
    PENDBASER [
        InnerCache OFFSET(7) NUMBITS(3) [
            NonCacheable = 0b001,
            WaWb = 0b111,
        ],
        OuterCache OFFSET(56) NUMBITS(3) [
            NonCacheable = 0b001,
            WaWb = 0b111,
        ],
        PTZ OFFSET(62) NUMBITS(1) [],
        PhysicalAddress OFFSET(16) NUMBITS(36) [],
    ],
];

#[allow(dead_code)]
impl LPI {
    /// Wake up the redistributor
    pub fn wake(&self) -> Result<(), &'static str> {
        self.WAKER.write(WAKER::ProcessorSleep::CLEAR);

        while self.WAKER.is_set(WAKER::ChildrenAsleep) {
            spin_loop();
        }

        self.wait_for_rwp()
    }

    pub fn wait_for_rwp(&self) -> Result<(), &'static str> {
        const MAX_RETRIES: u32 = 1000;
        let mut retries = 0;

        while self.CTLR.is_set(RCtrl::RWP) {
            if retries > MAX_RETRIES {
                return Err("Timeout waiting for register write to complete");
            }
            core::hint::spin_loop();
            retries += 1;
        }
        Ok(())
    }

    /// Enable LPI support
    pub fn enable_lpi(&self) {
        self.CTLR.modify(RCtrl::EnableLPIs::SET);
    }

    /// Configure property and pending tables for physical LPIs.
    pub fn configure_lpi_tables(
        &self,
        property_table_phys: u64,
        property_id_bits: u8,
        pending_table_phys: u64,
    ) -> Result<(), &'static str> {
        self.disable_lpi();
        self.PROPBASER.write(
            PROPBASER::IDbits.val(property_id_bits.saturating_sub(1) as u64)
                + PROPBASER::PhysicalAddress.val(property_table_phys >> 12)
                + PROPBASER::InnerCache::WaWb
                + PROPBASER::OuterCache::WaWb,
        );
        self.PENDBASER.write(
            PENDBASER::PhysicalAddress.val(pending_table_phys >> 16)
                + PENDBASER::InnerCache::WaWb
                + PENDBASER::OuterCache::WaWb,
        );
        self.enable_lpi();
        self.wait_for_rwp()
    }

    /// Disable LPI support
    pub fn disable_lpi(&self) {
        self.CTLR.modify(RCtrl::EnableLPIs::CLEAR);
        // Wait for register write to complete
        while self.CTLR.is_set(RCtrl::RWP) {
            spin_loop();
        }
    }

    /// Check if LPI is enabled
    pub fn is_lpi_enabled(&self) -> bool {
        self.CTLR.is_set(RCtrl::EnableLPIs)
    }

    /// Set LPI as pending
    pub fn set_lpi_pending(&self, intid: u32) {
        self.SETLPIR.set(intid as u64);
    }

    /// Clear LPI pending state
    pub fn clear_lpi_pending(&self, intid: u32) {
        self.CLRLPIR.set(intid as u64);
    }

    /// Invalidate LPI
    pub fn invalidate_lpi(&self, intid: u32) {
        self.INVLPIR.set(intid as u64);
    }

    /// Invalidate all LPIs
    pub fn invalidate_all_lpi(&self) {
        self.INVALLR.set(0);
    }

    /// Wait for synchronization
    pub fn sync(&self) {
        while self.SYNCR.get() != 0 {
            spin_loop();
        }
    }

    /// Check if this is the last redistributor
    pub fn is_last(&self) -> bool {
        self.TYPER.is_set(TYPER::Last)
    }

    /// Get affinity value
    pub fn get_affinity(&self) -> u32 {
        self.TYPER.read(TYPER::Affinity) as u32
    }

    pub fn processor_number(&self) -> u16 {
        self.TYPER.read(TYPER::ProcessorNumber) as u16
    }

    /// Check if physical LPIs are supported
    pub fn supports_physical_lpi(&self) -> bool {
        self.TYPER.is_set(TYPER::PLPIS)
    }

    /// Check if virtual LPIs are supported
    pub fn supports_virtual_lpi(&self) -> bool {
        self.TYPER.is_set(TYPER::VLPIS)
    }
}

register_structs! {
    #[allow(non_snake_case)]
    pub SGI {
        (0x0000 => _rsv0),
        (0x0080 => pub IGROUPR0: ReadWrite<u32>),
        (0x0084 => pub IGROUPR_E: [ReadWrite<u32>; 2]),
        (0x008C => _rsv1),
        (0x0100 => pub ISENABLER0: ReadWrite<u32>),
        (0x0104 => pub ISENABLER_E: [ReadWrite<u32>;2]),
        (0x010C => _rsv2),
        (0x0180 => pub ICENABLER0 : ReadWrite<u32>),
        (0x0184 => pub ICENABLER_E: [ReadWrite<u32>;2]),
        (0x018C => _rsv3),
        (0x0200 => pub ISPENDR0: ReadWrite<u32>),
        (0x0204 => pub ISPENDR_E: [ReadWrite<u32>; 2]),
        (0x020C => _rsv4),
        (0x0280 => pub ICPENDR0: ReadWrite<u32>),
        (0x0284 => pub ICPENDR_E: [ReadWrite<u32>; 2]),
        (0x028C => _rsv5),
        (0x0300 => pub ISACTIVER0: ReadWrite<u32>),
        (0x0304 => pub ISACTIVER_E: [ReadWrite<u32>; 2]),
        (0x030C => _rsv6),
        (0x0380 => pub ICACTIVER0: ReadWrite<u32>),
        (0x0384 => pub ICACTIVER_E: [ReadWrite<u32>; 2]),
        (0x038C => _rsv7),
        (0x0400 => pub IPRIORITYR: [ReadWrite<u8>; 32]),
        (0x0420 => pub IPRIORITYR_E: [ReadWrite<u8>; 64]),
        (0x0460 => _rsv8),
        (0x0C00 => pub ICFGR : [ReadWrite<u32>; 6]),
        (0x0C18 => _rsv9),
        (0x0D00 => pub IGRPMODR0 : ReadWrite<u32>),
        (0x0D04 => pub IGRPMODR_E: [ReadWrite<u32>;2]),
        (0x0D0C => _rsv10),
        (0x0E00 => pub NSACR: ReadWrite<u32>),
        (0x0E04 => _rsv11),
        (0x0F80 => pub INMIR0: ReadWrite<u32>),
        (0x0F84 => pub INMIR_E: [ReadWrite<u32>; 30]),
        (0x0FFC => _rsv12),
        (0x10000 => @END),
    }
}
#[allow(dead_code)]
impl SGI {
    /// Initialize SGI/PPI registers to a known state
    /// This is called during CPU interface initialization
    pub fn init_sgi_ppi(&self, security_state: crate::v3::SecurityState) {
        // Clear all pending interrupts first
        self.ICPENDR0.set(u32::MAX);

        // Disable all interrupts
        self.ICENABLER0.set(u32::MAX);

        // Clear all active interrupts
        self.ICACTIVER0.set(u32::MAX);

        // Configure interrupt groups based on security state
        match security_state {
            crate::v3::SecurityState::Single => {
                // In single security state, all interrupts go to Group 1
                self.IGROUPR0.set(u32::MAX);
                self.IGRPMODR0.set(0);
            }
            crate::v3::SecurityState::Secure => {
                // In secure state, configure for both Group 0 and Group 1
                // SGIs (0-15) typically to Group 0, PPIs (16-31) to Group 1
                self.IGROUPR0.set(0xFFFF0000);
                self.IGRPMODR0.set(0);
            }
            crate::v3::SecurityState::NonSecure => {
                // In non-secure state, all interrupts go to Group 1
                self.IGROUPR0.set(u32::MAX);
                self.IGRPMODR0.set(0);
            }
        }

        // Set default priorities (lower priority = higher urgency)
        for i in 0..32 {
            self.IPRIORITYR[i].set(0xA0); // Default to middle priority
        }
    }

    /// Set interrupt enable state
    pub fn set_enable_interrupt(&self, irq: IntId, enable: bool) {
        let int_id: u32 = irq.into();
        let bit = 1 << (int_id % 32);
        if enable {
            self.ISENABLER0.set(bit);
        } else {
            self.ICENABLER0.set(bit);
        }
    }

    pub fn is_interrupt_enabled(&self, irq: IntId) -> bool {
        let int_id: u32 = irq.into();
        let bit = 1 << (int_id % 32);
        (self.ISENABLER0.get() & bit) != 0
    }

    /// Set interrupt priority
    pub fn set_priority(&self, intid: IntId, priority: u8) {
        self.IPRIORITYR[u32::from(intid) as usize].set(priority)
    }

    pub fn get_priority(&self, intid: IntId) -> u8 {
        self.IPRIORITYR[u32::from(intid) as usize].get()
    }

    /// Set interrupt configuration (edge/level triggered)
    pub fn set_cfgr(&self, intid: IntId, trigger: Trigger) {
        let int_id = intid.to_u32();
        let bit_offset = (int_id % 16) * 2 + 1; // Each interrupt uses 2 bits, we use bit 1 for edge/level
        let clean = !(1u32 << bit_offset);
        let bit: u32 = match trigger {
            Trigger::Edge => 1,
            Trigger::Level => 0,
        } << bit_offset;

        if intid.is_sgi() {
            let mut mask = self.ICFGR[0].get();
            mask &= clean;
            mask |= bit;
            self.ICFGR[0].set(mask);
        } else {
            let mut mask = self.ICFGR[1].get();
            mask &= clean;
            mask |= bit;
            self.ICFGR[1].set(mask);
        }
    }

    pub fn get_cfgr(&self, intid: IntId) -> Trigger {
        let int_id = intid.to_u32();
        let bit_offset = (int_id % 16) * 2 + 1; // Each interrupt uses 2 bits, we use bit 1 for edge/level
        let mask = 1u32 << bit_offset;
        if intid.is_sgi() {
            if self.ICFGR[0].get() & mask != 0 {
                Trigger::Edge
            } else {
                Trigger::Level
            }
        } else if self.ICFGR[1].get() & mask != 0 {
            Trigger::Edge
        } else {
            Trigger::Level
        }
    }

    /// Set interrupt pending state
    pub fn set_pending(&self, intid: IntId, pending: bool) {
        let int_id: u32 = intid.into();
        let bit = 1 << (int_id % 32);
        if pending {
            self.ISPENDR0.set(bit);
        } else {
            self.ICPENDR0.set(bit);
        }
    }

    pub fn is_pending(&self, intid: IntId) -> bool {
        let int_id: u32 = intid.into();
        let bit = 1 << (int_id % 32);
        (self.ISPENDR0.get() & bit) != 0
    }

    /// Set interrupt active state
    pub fn set_active(&self, intid: IntId, active: bool) {
        let int_id: u32 = intid.into();
        let bit = 1 << (int_id % 32);
        if active {
            self.ISACTIVER0.set(bit);
        } else {
            self.ICACTIVER0.set(bit);
        }
    }

    pub fn is_active(&self, intid: IntId) -> bool {
        let int_id: u32 = intid.into();
        let bit = 1 << (int_id % 32);
        (self.ISACTIVER0.get() & bit) != 0
    }

    /// Set interrupt group
    pub fn set_group(&self, intid: IntId, group1: bool) {
        let int_id: u32 = intid.into();
        let bit = 1 << (int_id % 32);
        if group1 {
            self.IGROUPR0.set(self.IGROUPR0.get() | bit);
        } else {
            self.IGROUPR0.set(self.IGROUPR0.get() & !bit);
        }
    }

    pub fn is_group1(&self, intid: IntId) -> bool {
        let int_id: u32 = intid.into();
        let bit = 1 << (int_id % 32);
        (self.IGROUPR0.get() & bit) != 0
    }

    /// Set interrupt group modifier
    pub fn set_group_modifier(&self, intid: IntId, modifier: bool) {
        let int_id: u32 = intid.into();
        let bit = 1 << (int_id % 32);
        if modifier {
            self.IGRPMODR0.set(self.IGRPMODR0.get() | bit);
        } else {
            self.IGRPMODR0.set(self.IGRPMODR0.get() & !bit);
        }
    }
}

register_bitfields! [
    u64,
    pub TYPER [
        /// Indicates whether the GIC implementation supports physical LPIs.
        PLPIS OFFSET(0) NUMBITS(1) [],
        /// Indicates whether the Redistributor supports virtual LPIs.
        VLPIS OFFSET(1) NUMBITS(1) [],
        /// Indicates whether the Redistributor is DirtyLPI-capable.
        Dirty OFFSET(2) NUMBITS(1) [],
        /// Indicates whether this Redistributor is the last in the series of Redistributors.
        Last OFFSET(4) NUMBITS(1) [],
        /// Indicates whether the Redistributor supports Direct injection of LPIs.
        DirectLPI OFFSET(3) NUMBITS(1) [],
        /// Common LPI Affinity
        CommonLPIAff OFFSET(24) NUMBITS(2) [],
        /// Processor Number
        ProcessorNumber OFFSET(8) NUMBITS(16) [],
        /// Affinity value
        Affinity OFFSET(32) NUMBITS(32) [],
    ],

    pub IROUTER [
        AFF0 OFFSET(0) NUMBITS(8) [],
        AFF1 OFFSET(8) NUMBITS(8) [],
        AFF2 OFFSET(16) NUMBITS(8) [],
        InterruptRoutingMode OFFSET(31) NUMBITS(1) [
            Aff=0,
            Any=1,
        ],
        AFF3 OFFSET(32) NUMBITS(8) [],
    ]
];
register_bitfields! [
    u32,
    WAKER [
        ProcessorSleep OFFSET(1) NUMBITS(1) [],
        ChildrenAsleep OFFSET(2) NUMBITS(1) [],
    ],
    CTLR_TWO_S [
        EnableGrp0 OFFSET(0) NUMBITS(1) [],
        EnableGrp1NS OFFSET(1) NUMBITS(1) [],
        EnableGrp1S OFFSET(2) NUMBITS(1) [],
        ARE_S OFFSET(4) NUMBITS(1) [],
        ARE_NS OFFSET(5) NUMBITS(1) [],
        DS OFFSET(6) NUMBITS(1) [],
        RWP OFFSET(31) NUMBITS(1) [],
    ],
    CTLR_TWO_NS [
        EnableGrp1 OFFSET(0) NUMBITS(1) [],
        EnableGrp1A OFFSET(1) NUMBITS(1) [],
        ARE_NS OFFSET(4) NUMBITS(1) [],
        RWP OFFSET(31) NUMBITS(1) [],
    ],
    CTLR_ONE_NS [
        EnableGrp0 OFFSET(0) NUMBITS(1) [],
        EnableGrp1 OFFSET(1) NUMBITS(1) [],
        ARE OFFSET(4) NUMBITS(1) [],
        DS OFFSET(6) NUMBITS(1) [],
        RWP OFFSET(31) NUMBITS(1) [],
    ],
];
