//! AArch64 CPU identity.

/// Architectural MIDR_EL1 identity, independent of platform CPU numbering.
/// Cluster grouping and Linux PMU source names belong to the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Midr(u64);

impl Midr {
    /// Reads this CPU's architectural identity.
    pub fn read() -> Self {
        Self(read_midr_el1())
    }

    /// Wraps a previously captured identity without accessing hardware.
    pub const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    /// Returns the complete captured MIDR value.
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// Returns the implementer code in bits 31:24.
    pub const fn implementer(self) -> u8 {
        (self.0 >> 24) as u8
    }

    /// Returns the implementation's part number in bits 15:4.
    pub const fn part_number(self) -> u16 {
        ((self.0 >> 4) & 0xfff) as u16
    }

    /// Returns the major revision in bits 23:20.
    pub const fn variant(self) -> u8 {
        ((self.0 >> 20) & 15) as u8
    }

    /// Returns the minor revision in bits 3:0.
    pub const fn revision(self) -> u8 {
        (self.0 & 15) as u8
    }
}

/// Reads the raw MIDR_EL1 identity of the current CPU.
pub fn read_midr_el1() -> u64 {
    let value;
    // SAFETY: callers of CPU primitives execute in privileged kernel context.
    unsafe {
        core::arch::asm!("mrs {}, MIDR_EL1", out(reg) value, options(nomem, nostack));
    }
    value
}

/// Raw architectural feature registers used by upper-layer capability policy.
/// Values are not filtered for any operating system's supported user context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdRegister {
    /// Processor features, including implemented exception levels and FP/SIMD.
    Pfr0,
    /// Instruction-set attributes, first group.
    Isar0,
    /// Instruction-set attributes, second group.
    Isar1,
    /// Memory-model attributes, first group.
    Mmfr0,
    /// Memory-model attributes, second group.
    Mmfr1,
    /// Memory-model attributes, third group.
    Mmfr2,
}

impl IdRegister {
    /// Reads this CPU's unmodified architectural feature value at a privileged EL.
    pub fn read(self) -> u64 {
        let value;
        // SAFETY: the CPU API is used in privileged context; these architectural
        // identification registers are read-only and do not transfer ownership.
        unsafe {
            match self {
                Self::Pfr0 => {
                    core::arch::asm!("mrs {}, ID_AA64PFR0_EL1", out(reg) value, options(nomem, nostack))
                }
                Self::Isar0 => {
                    core::arch::asm!("mrs {}, ID_AA64ISAR0_EL1", out(reg) value, options(nomem, nostack))
                }
                Self::Isar1 => {
                    core::arch::asm!("mrs {}, ID_AA64ISAR1_EL1", out(reg) value, options(nomem, nostack))
                }
                Self::Mmfr0 => {
                    core::arch::asm!("mrs {}, ID_AA64MMFR0_EL1", out(reg) value, options(nomem, nostack))
                }
                Self::Mmfr1 => {
                    core::arch::asm!("mrs {}, ID_AA64MMFR1_EL1", out(reg) value, options(nomem, nostack))
                }
                Self::Mmfr2 => {
                    core::arch::asm!("mrs {}, ID_AA64MMFR2_EL1", out(reg) value, options(nomem, nostack))
                }
            }
        }
        value
    }
}

/// Returns the implemented physical-address width from ID_AA64MMFR0_EL1.
/// Reserved encodings return None; consumers choose their supported format cap.
pub fn physical_address_bits() -> Option<usize> {
    match IdRegister::Mmfr0.read() & 15 {
        0 => Some(32),
        1 => Some(36),
        2 => Some(40),
        3 => Some(42),
        4 => Some(44),
        5 => Some(48),
        6 => Some(52),
        7 => Some(56),
        _ => None,
    }
}
