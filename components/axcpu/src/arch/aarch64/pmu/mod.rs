//! PMUv3 hardware access. Linux reference: v7.1 arm_pmuv3.c at
//! 8cd9520d35a6c38db6567e97dd93b1f11f185dc6.

use core::{arch::asm, marker::PhantomData};

mod capability;
mod registers;
pub use capability::{EventSupport, PmuInfo};

macro_rules! read_reg {
    ($register:literal) => {{
        let value: u64;
        // SAFETY: the enclosing PMU session owns this CPU's register access.
        unsafe { asm!(concat!("mrs {}, ", $register), out(reg) value, options(nomem, nostack)); }
        value
    }};
}

macro_rules! write_reg {
    ($register:literal, $value:expr) => {{
        // SAFETY: the session has exclusive access and supplies defined bits.
        unsafe { asm!(concat!("msr ", $register, ", {}"), in(reg) $value as u64, options(nostack)); }
    }};
}

mod access;
mod counter;
mod overflow;

fn isb() {
    // SAFETY: instruction synchronization does not access memory.
    unsafe {
        asm!("isb", options(nostack));
    }
}

/// A requested PMU operation could not be performed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PmuError {
    /// This CPU does not implement architectural PMUv3.
    #[error("PMUv3 is unavailable on this CPU")]
    Unavailable,
    /// The counter index is not implemented on this CPU.
    #[error("counter is not implemented on this CPU")]
    InvalidCounter,
    /// The event is architecturally reported as unsupported.
    #[error("event is reported as unsupported")]
    UnsupportedEvent,
    /// The configuration does not apply to the selected counter.
    #[error("configuration does not apply to this counter")]
    InvalidConfiguration,
    /// The requested period is zero or exceeds the active counter width.
    #[error("period is zero or exceeds the counter width")]
    InvalidPeriod,
}

/// Validated hardware counter selector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CounterId(u8);

impl CounterId {
    /// The dedicated cycle counter at architectural index 31.
    pub const CYCLE: Self = Self(31);

    /// Dedicated PMICNTR at architectural index 32, when independently present.
    pub const INSTRUCTIONS: Self = Self(32);

    /// Returns the architectural index, also used in overflow masks.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// PMUv3 event code and privilege filters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventConfig {
    /// Raw architectural or implementation-defined event code.
    pub event: u16,
    /// Excludes execution at EL0.
    pub exclude_user: bool,
    /// Excludes execution at EL1.
    pub exclude_kernel: bool,
    /// Includes execution at EL2.
    pub include_hypervisor: bool,
}

/// Exclusive access to the current CPU's PMU for one non-migrating scope.
/// This object does not allocate counters or implicitly reset them on drop.
pub struct Pmu {
    info: PmuInfo,
    _not_send_sync: PhantomData<*mut ()>,
}

impl Pmu {
    /// Probes PMUv3 without changing hardware state.
    ///
    /// # Safety
    /// The caller must run at a privileged level with PMU access permitted by
    /// higher exception levels. It must keep this CPU pinned and serialize all
    /// PMU access, including local IRQs, for the complete returned session.
    pub unsafe fn current() -> Result<Self, PmuError> {
        let version = ((read_reg!("ID_AA64DFR0_EL1") >> 8) & 15) as u8;
        if version == 0 || version == 15 {
            return Err(PmuError::Unavailable);
        }
        let pmcr = read_reg!("PMCR_EL0");
        Ok(Self {
            info: PmuInfo {
                version,
                num_counters: ((pmcr >> 11) & 31) as usize,
                has_instruction_counter: (read_reg!("ID_AA64DFR1_EL1") >> 36) & 15 != 0,
                counter_width: if version >= 6 && pmcr & (1 << 7) != 0 {
                    64
                } else {
                    32
                },
                cycle_counter_width: if pmcr & (1 << 6) != 0 { 64 } else { 32 },
                pmceid0: read_reg!("PMCEID0_EL0"),
                pmceid1: read_reg!("PMCEID1_EL0"),
            },
            _not_send_sync: PhantomData,
        })
    }

    /// Returns the capability snapshot for this session's CPU.
    pub const fn info(&self) -> PmuInfo {
        self.info
    }

    /// Validates a programmable counter index on this CPU.
    pub fn counter(&self, index: usize) -> Result<CounterId, PmuError> {
        if index < self.info.num_counters {
            Ok(CounterId(index as u8))
        } else {
            Err(PmuError::InvalidCounter)
        }
    }

    fn validate(&self, id: CounterId) -> Result<(), PmuError> {
        if id == CounterId::CYCLE
            || (id == CounterId::INSTRUCTIONS && self.info.has_instruction_counter)
            || id.index() < self.info.num_counters
        {
            Ok(())
        } else {
            Err(PmuError::InvalidCounter)
        }
    }

    /// Returns the active overflow width of this counter.
    pub fn width(&self, id: CounterId) -> Result<u8, PmuError> {
        self.validate(id)?;
        Ok(if id == CounterId::INSTRUCTIONS {
            64
        } else if id == CounterId::CYCLE {
            self.info.cycle_counter_width
        } else {
            self.info.counter_width
        })
    }

    fn mask(&self, id: CounterId) -> Result<u64, PmuError> {
        Ok(u64::MAX >> (64 - self.width(id)?))
    }

    fn implemented_mask(&self) -> u64 {
        ((1u64 << self.info.num_counters) - 1)
            | (1u64 << 31)
            | (u64::from(self.info.has_instruction_counter) << 32)
    }

    /// Returns whether global PMU counting is enabled on this CPU.
    pub fn is_running(&self) -> bool {
        read_reg!("PMCR_EL0") & 1 != 0
    }

    /// Pauses global counting for a bounded snapshot and restores its prior state.
    /// Counter enables, values, overflow state and user permissions are retained.
    ///
    /// # Safety
    /// The caller must own the complete local PMU scheduling domain: pausing
    /// must be permitted for every configured event. The callback must not
    /// block, enable IRQs, migrate, or create another PMU access session.
    pub unsafe fn with_counting_paused<R>(&mut self, operation: impl FnOnce(&mut Self) -> R) -> R {
        struct Restore<'a> {
            pmu: &'a mut Pmu,
            running: bool,
        }
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                if self.running {
                    self.pmu.start();
                } else {
                    self.pmu.stop();
                }
            }
        }
        let running = self.is_running();
        self.stop();
        let restore = Restore { pmu: self, running };
        operation(restore.pmu)
    }

    /// Enables global counting without resetting or reallocating any counter.
    pub fn start(&mut self) {
        let value = (read_reg!("PMCR_EL0") & 0xf9) | 1;
        isb();
        write_reg!("PMCR_EL0", value);
    }

    /// Stops global counting without changing counter values.
    pub fn stop(&mut self) {
        let value = read_reg!("PMCR_EL0") & 0xf8;
        isb();
        write_reg!("PMCR_EL0", value);
        isb();
    }

    /// Resets all counters while globally stopped and disables user access.
    /// Long programmable counters are enabled when PMUv3p5 supports them.
    ///
    /// # Safety
    /// The caller must own every counter on this CPU and have withdrawn all
    /// event users. No active perf or guest owner may retain counter state.
    pub unsafe fn reset(&mut self) {
        let mask = self.implemented_mask();
        write_reg!("PMCNTENCLR_EL0", mask);
        isb();
        write_reg!("PMINTENCLR_EL1", mask);
        isb();
        write_reg!("PMOVSCLR_EL0", mask);
        isb();
        write_reg!("PMUSERENR_EL0", 0);
        let value = 2
            | 4
            | 64
            | if self.info.has_long_counters() {
                128
            } else {
                0
            };
        isb();
        write_reg!("PMCR_EL0", value);
        isb();
        if self.info.has_instruction_counter {
            write_reg!("S3_3_C9_C4_0", 0); // PMICNTR_EL0
        }
        self.info.counter_width = if self.info.has_long_counters() {
            64
        } else {
            32
        };
        self.info.cycle_counter_width = 64;
    }
}
