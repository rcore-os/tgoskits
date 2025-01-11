pub mod lvt;
mod svr;
pub use svr::*;

use tock_registers::register_structs;
use tock_registers::registers::{ReadOnly, ReadWrite, WriteOnly};

use lvt::{
    LvtCmciRegisterMmio, LvtErrorRegisterMmio, LvtLint0RegisterMmio, LvtLint1RegisterMmio,
    LvtPerformanceCounterRegisterMmio, LvtThermalMonitorRegisterMmio, LvtTimerRegisterMmio,
};

register_structs! {
    #[allow(non_snake_case)]
    pub LocalAPICRegs {
        (0x00 => _reserved0),
        /// Local APIC ID register (VID): the 32-bit field located at offset 000H on the virtual-APIC page.
        (0x20 => pub ID: ReadWrite<u32>),
        (0x24 => _reserved1),
        /// Local APIC Version register (VVER): the 32-bit field located at offset 030H on the virtual-APIC page.
        (0x30 => pub VERSION: ReadOnly<u32>),
        (0x34 => _reserved2),
        /// Virtual task-priority register (VTPR): the 32-bit field located at offset 080H on the virtual-APIC page.
        (0x80 => pub TPR: ReadWrite<u32>),
        (0x84 => _reserved3),
        /// Virtual APIC-priority register (VAPR): the 32-bit field located at offset 090H on the virtual-APIC page.
        (0x90 => pub APR: ReadOnly<u32>),
        (0x94 => _reserved4),
        /// Virtual processor-priority register (VPPR): the 32-bit field located at offset 0A0H on the virtual-APIC page.
        (0xA0 => pub PPR: ReadOnly<u32>),
        (0xA4 => _reserved5),
        /// Virtual end-of-interrupt register (VEOI): the 32-bit field located at offset 0B0H on the virtual-APIC page.
        (0xB0 => pub EOI: WriteOnly<u32>),
        (0xB4 => _reserved6),
        /// Virtual Remote Read Register (RRD): the 32-bit field located at offset 0C0H on the virtual-APIC page.
        (0xC0 => pub RRD: ReadOnly<u32>),
        (0xC4 => _reserved7),
        /// Virtual Logical Destination Register (LDR): the 32-bit field located at offset 0D0H on the virtual-APIC page.
        (0xD0 => pub LDR: ReadWrite<u32>),
        (0xD4 => _reserved8),
        /// Virtual Destination Format Register (DFR): the 32-bit field located at offset 0E0H on the virtual-APIC page.
        (0xE0 => pub DFR: ReadWrite<u32>),
        (0xE4 => _reserved9),
        /// Virtual Spurious Interrupt Vector Register (SVR): the 32-bit field located at offset 0F0H on the virtual-APIC page.
        (0xF0 => pub SVR: SpuriousInterruptVectorRegisterMmio),
        (0xF4 => _reserved10),
        /// Virtual interrupt-service register (VISR):
        /// the 256-bit value comprising eight non-contiguous 32-bit fields at offsets
        /// 100H, 110H, 120H, 130H, 140H, 150H, 160H, and 170H on the virtual-APIC page.
        (0x100 => pub ISR: [ReadOnly<u128>; 8]),
        /// Virtual trigger-mode register (VTMR):
        /// the 256-bit value comprising eight non-contiguous 32-bit fields at offsets
        /// 180H, 190H, 1A0H, 1B0H, 1C0H, 1D0H, 1E0H, and 1F0H on the virtual-APIC page.
        (0x180 => pub TMR: [ReadOnly<u128>; 8]),
        /// Virtual interrupt-request register (VIRR):
        /// the 256-bit value comprising eight non-contiguous 32-bit fields at offsets
        /// 200H, 210H, 220H, 230H, 240H, 250H, 260H, and 270H on the virtual-APIC page.
        /// Bit x of the VIRR is at bit position (x & 1FH) at offset (200H | ((x & E0H) » 1)).
        /// The processor uses only the low 4 bytes of each of the 16-Byte fields at offsets 200H, 210H, 220H, 230H, 240H, 250H, 260H, and 270H.
        (0x200 => pub IRR: [ReadOnly<u128>; 8]),
        /// Virtual error-status register (VESR): the 32-bit field located at offset 280H on the virtual-APIC page.
        (0x280 => pub ESR: ReadWrite<u32>),
        (0x284 => _reserved11),
        /// Virtual LVT Corrected Machine Check Interrupt (CMCI) Register
        (0x2F0 => pub LVT_CMCI: LvtCmciRegisterMmio),
        (0x2F4 => _reserved12),
        /// Virtual Interrupt Command Register (ICR): the 64-bit field located at offset 300H on the virtual-APIC page.
        (0x300 => pub ICR_LO: ReadWrite<u32>),
        (0x304 => _reserved13),
        (0x310 => pub ICR_HI: ReadWrite<u32>),
        (0x314 => _reserved14),
        /// Virtual LVT Timer Register: the 32-bit field located at offset 320H on the virtual-APIC page.
        (0x320 => pub LVT_TIMER: LvtTimerRegisterMmio),
        (0x324 => _reserved15),
        /// Virtual LVT Thermal Sensor register: the 32-bit field located at offset 330H on the virtual-APIC page.
        (0x330 => pub LVT_THERMAL: LvtThermalMonitorRegisterMmio),
        (0x334 => _reserved16),
        /// Virtual LVT Performance Monitoring Counters register: the 32-bit field located at offset 340H on the virtual-APIC page.
        (0x340 => pub LVT_PMI: LvtPerformanceCounterRegisterMmio),
        (0x344 => _reserved17),
        /// Virtual LVT LINT0 register: the 32-bit field located at offset 350H on the virtual-APIC page.
        (0x350 => pub LVT_LINT0: LvtLint0RegisterMmio),
        (0x354 => _reserved18),
        /// Virtual LVT LINT1 register: the 32-bit field located at offset 360H on the virtual-APIC page.
        (0x360 => pub LVT_LINT1: LvtLint1RegisterMmio),
        (0x364 => _reserved19),
        /// Virtual LVT Error register: the 32-bit field located at offset 370H on the virtual-APIC page.
        (0x370 => pub LVT_ERROR: LvtErrorRegisterMmio),
        (0x374 => _reserved20),
        /// Virtual Initial Count Register (for Timer): the 32-bit field located at offset 380H on the virtual-APIC page.
        (0x380 => pub ICR_TIMER: ReadWrite<u32>),
        (0x384 => _reserved21),
        /// Virtual Current Count Register (for Timer): the 32-bit field located at offset 390H on the virtual-APIC page.
        (0x390 => pub CCR_TIMER: ReadOnly<u32>),
        (0x394 => _reserved22),
        /// Virtual Divide Configuration Register (for Timer): the 32-bit field located at offset 3E0H on the virtual-APIC page.
        (0x3E0 => pub DCR_TIMER: ReadWrite<u32>),
        (0x3E4 => _reserved23),
        /// Virtual SELF IPI Register: the 32-bit field located at offset 3F0H on the virtual-APIC page.
        (0x3F0 => pub SELF_IPI: WriteOnly<u32>),
        (0x3F4 => _reserved24),
        (0x1000 => @END),
    }
}
