use paste::paste;

macro_rules! define_index_enum {
    ($name:ident) => {
        paste! {
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            pub enum $name {
                [<$name 0>] = 0,
                [<$name 1>] = 1,
                [<$name 2>] = 2,
                [<$name 3>] = 3,
                [<$name 4>] = 4,
                [<$name 5>] = 5,
                [<$name 6>] = 6,
                [<$name 7>] = 7,
            }

            impl $name {
                const fn from(value: usize) -> Self {
                    match value {
                        0 => $name::[<$name 0>],
                        1 => $name::[<$name 1>],
                        2 => $name::[<$name 2>],
                        3 => $name::[<$name 3>],
                        4 => $name::[<$name 4>],
                        5 => $name::[<$name 5>],
                        6 => $name::[<$name 6>],
                        7 => $name::[<$name 7>],
                        _ => panic!("Invalid index"),
                    }
                }

                pub const fn as_usize(&self) -> usize {
                    *self as usize
                }
            }

            impl core::fmt::Display for $name {
                fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                    match self {
                        $name::[<$name 0>] => write!(f, "{}0", stringify!($name)),
                        $name::[<$name 1>] => write!(f, "{}1", stringify!($name)),
                        $name::[<$name 2>] => write!(f, "{}2", stringify!($name)),
                        $name::[<$name 3>] => write!(f, "{}3", stringify!($name)),
                        $name::[<$name 4>] => write!(f, "{}4", stringify!($name)),
                        $name::[<$name 5>] => write!(f, "{}5", stringify!($name)),
                        $name::[<$name 6>] => write!(f, "{}6", stringify!($name)),
                        $name::[<$name 7>] => write!(f, "{}7", stringify!($name)),
                    }
                }
            }
        }
    };
}

define_index_enum!(ISRIndex);
define_index_enum!(TMRIndex);
define_index_enum!(IRRIndex);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::upper_case_acronyms)]
pub enum ApicRegOffset {
    /// ID register 0x2.
    ID,
    /// Version register 0x3.
    Version,
    /// Task Priority register 0x8.
    TPR,
    /// Arbitration Priority register 0x9.
    APR,
    /// Processor Priority register 0xA.
    PPR,
    /// EOI register 0xB.
    EOI,
    /// Remote Read register 0xC.
    RRR,
    /// Logical Destination Register 0xD.
    LDR,
    /// Destination Format register 0xE.
    DFR,
    /// Spurious Interrupt Vector register 0xF.
    SIVR,
    /// In-Service register 0x10..=0x17.
    ISR(ISRIndex),
    /// Trigger Mode register 0x18..=0x1F.
    TMR(TMRIndex),
    /// Interrupt Request register 0x20..=0x27.
    IRR(IRRIndex),
    /// Error Status register 0x28.
    ESR,
    /// LVT CMCI register 0x2F.
    LvtCMCI,
    /// Interrupt Command register 0x30.
    ICRLow,
    /// Interrupt Command register high 0x30.
    ICRHi,
    /// LVT Timer Interrupt register 0x32.
    LvtTimer,
    /// LVT Thermal Sensor Interrupt register 0x33.
    LvtThermal,
    /// LVT Performance Monitoring Counters Register 0x34.
    LvtPmc,
    /// LVT LINT0 register 0x35.
    LvtLint0,
    /// LVT LINT1 register 0x36.
    LvtLint1,
    /// LVT Error register 0x37.
    LvtErr,
    /// Initial Count register (for Timer) 0x38.
    TimerInitCount,
    /// Current Count register (for Timer) 0x39.
    TimerCurCount,
    /// Divide Configuration register (for Timer) 0x3E.
    TimerDivConf,
    /// Self IPI register 0x3F.
    /// Available only in x2APIC mode.
    SelfIPI,
}

impl ApicRegOffset {
    const fn from(value: usize) -> Self {
        match value as u32 {
            0x2 => ApicRegOffset::ID,
            0x3 => ApicRegOffset::Version,
            0x8 => ApicRegOffset::TPR,
            0x9 => ApicRegOffset::APR,
            0xA => ApicRegOffset::PPR,
            0xB => ApicRegOffset::EOI,
            0xC => ApicRegOffset::RRR,
            0xD => ApicRegOffset::LDR,
            0xE => ApicRegOffset::DFR,
            0xF => ApicRegOffset::SIVR,
            0x10..=0x17 => ApicRegOffset::ISR(ISRIndex::from(value - 0x10)),
            0x18..=0x1F => ApicRegOffset::TMR(TMRIndex::from(value - 0x18)),
            0x20..=0x27 => ApicRegOffset::IRR(IRRIndex::from(value - 0x20)),
            0x28 => ApicRegOffset::ESR,
            0x2F => ApicRegOffset::LvtCMCI,
            0x30 => ApicRegOffset::ICRLow,
            0x31 => ApicRegOffset::ICRHi,
            0x32 => ApicRegOffset::LvtTimer,
            0x33 => ApicRegOffset::LvtThermal,
            0x34 => ApicRegOffset::LvtPmc,
            0x35 => ApicRegOffset::LvtLint0,
            0x36 => ApicRegOffset::LvtLint1,
            0x37 => ApicRegOffset::LvtErr,
            0x38 => ApicRegOffset::TimerInitCount,
            0x39 => ApicRegOffset::TimerCurCount,
            0x3E => ApicRegOffset::TimerDivConf,
            0x3F => ApicRegOffset::SelfIPI,
            _ => panic!("Invalid APIC register offset"),
        }
    }
}

impl core::fmt::Display for ApicRegOffset {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ApicRegOffset::ID => write!(f, "ID"),
            ApicRegOffset::Version => write!(f, "Version"),
            ApicRegOffset::TPR => write!(f, "TPR"),
            ApicRegOffset::APR => write!(f, "APR"),
            ApicRegOffset::PPR => write!(f, "PPR"),
            ApicRegOffset::EOI => write!(f, "EOI"),
            ApicRegOffset::RRR => write!(f, "RRR"),
            ApicRegOffset::LDR => write!(f, "LDR"),
            ApicRegOffset::DFR => write!(f, "DFR"),
            ApicRegOffset::SIVR => write!(f, "SIVR"),
            ApicRegOffset::ISR(index) => write!(f, "{index:?}"),
            ApicRegOffset::TMR(index) => write!(f, "{index:?}"),
            ApicRegOffset::IRR(index) => write!(f, "{index:?}"),
            ApicRegOffset::ESR => write!(f, "ESR"),
            ApicRegOffset::LvtCMCI => write!(f, "LvtCMCI"),
            ApicRegOffset::ICRLow => write!(f, "ICR_LOW"),
            ApicRegOffset::ICRHi => write!(f, "ICR_HI"),
            ApicRegOffset::LvtTimer => write!(f, "LvtTimer"),
            ApicRegOffset::LvtThermal => write!(f, "LvtThermal"),
            ApicRegOffset::LvtPmc => write!(f, "LvtPerformanceMonitoringCounter"),
            ApicRegOffset::LvtLint0 => write!(f, "LvtLint0"),
            ApicRegOffset::LvtLint1 => write!(f, "LvtLint1"),
            ApicRegOffset::LvtErr => write!(f, "LvtErr"),
            ApicRegOffset::TimerInitCount => write!(f, "TimerInitCount"),
            ApicRegOffset::TimerCurCount => write!(f, "TimerCurCount"),
            ApicRegOffset::TimerDivConf => write!(f, "TimerDivConf"),
            ApicRegOffset::SelfIPI => write!(f, "SelfIPI"),
        }
    }
}

pub const APIC_LVT_M: u32 = 0x00010000;
pub const APIC_LVT_DS: u32 = 0x00001000;
pub const APIC_LVT_VECTOR: u32 = 0x000000ff;

/// 11.5.1 Local Vector Table
/// Figure 11-8. Local Vector Table (LVT)
/// - Value After Reset: 0001 0000H
pub const RESET_LVT_REG: u32 = APIC_LVT_M;
/// 11.9 SPURIOUS INTERRUPT
/// - Address: FEE0 00F0H
/// - Value after reset: 0000 00FFH
pub const RESET_SPURIOUS_INTERRUPT_VECTOR: u32 = 0x0000_00FF;

#[allow(dead_code)]
pub const LAPIC_TRIG_LEVEL: bool = true;
pub const LAPIC_TRIG_EDGE: bool = false;

pub mod xapic {
    use axaddrspace::GuestPhysAddr;

    use super::ApicRegOffset;

    pub const DEFAULT_APIC_BASE: usize = 0xFEE0_0000;
    pub const APIC_MMIO_SIZE: usize = 0x1000;

    pub const XAPIC_BROADCAST_DEST_ID: u32 = 0xFF;

    pub(crate) const fn xapic_mmio_access_reg_offset(addr: GuestPhysAddr) -> ApicRegOffset {
        ApicRegOffset::from((addr.as_usize() & (APIC_MMIO_SIZE - 1)) >> 4)
    }
}

pub mod x2apic {
    use axaddrspace::device::SysRegAddr;

    use super::ApicRegOffset;

    pub const X2APIC_MSE_REG_BASE: usize = 0x800;
    pub const X2APIC_MSE_REG_SIZE: usize = 0x100;

    /// A destination ID value of FFFF_FFFFH is used for broadcast of interrupts
    /// in both logical destination and physical destination modes.
    pub const X2APIC_BROADCAST_DEST_ID: u32 = 0xFFFF_FFFF;

    pub(crate) const fn x2apic_msr_access_reg(addr: SysRegAddr) -> ApicRegOffset {
        ApicRegOffset::from(addr.addr() - X2APIC_MSE_REG_BASE)
    }
}
