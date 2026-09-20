//! CPU generic-timer counters and comparator banks.

use core::{arch::asm, marker::PhantomData};

macro_rules! read_register {
    ($name:literal) => {{
        let value: u64;
        // SAFETY: the caller has access to this architectural register bank.
        unsafe { asm!(concat!("mrs {}, ", $name), out(reg) value, options(nomem, nostack)); }
        value
    }};
}

macro_rules! write_register {
    ($name:literal, $value:expr) => {{
        // SAFETY: the timer session owns the current CPU's comparator bank.
        unsafe { asm!(concat!("msr ", $name, ", {}"), in(reg) $value, options(nostack)); }
    }};
}

/// Reads the frequency advertised by CNTFRQ_EL0, in Hz.
/// Platform calibration and validation of this value remain with the caller.
pub fn counter_frequency() -> u64 {
    read_register!("CNTFRQ_EL0")
}

/// Reads the physical system counter after instruction synchronization.
pub fn physical_counter() -> u64 {
    synchronize();
    read_register!("CNTPCT_EL0")
}

/// Reads the virtual system counter, including the installed virtual offset.
pub fn virtual_counter() -> u64 {
    synchronize();
    read_register!("CNTVCT_EL0")
}

/// A hardware comparator bank; selecting a bank does not select an IRQ route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerKind {
    /// Non-secure physical timer, CNTP_*_EL0.
    Physical,
    /// Virtual timer, CNTV_*_EL0.
    Virtual,
    /// Non-VHE hypervisor physical timer, CNTHP_*_EL2.
    HypervisorPhysical,
}

bitflags::bitflags! {
    /// Generic timer control and read-only condition flags.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct TimerControl: u64 {
        /// Enables the comparator.
        const ENABLE = 1;
        /// Masks the timer's interrupt output without stopping the counter.
        const MASKED = 1 << 1;
        /// Read-only comparator condition; meaningful while enabled.
        const PENDING = 1 << 2;
    }
}

/// Exclusive, non-migrating access to one CPU timer comparator.
/// Dropping this view does not change timer or interrupt state.
#[derive(Debug)]
pub struct Timer {
    kind: TimerKind,
    _not_send_sync: PhantomData<*mut ()>,
}

impl Timer {
    /// Borrows a comparator without changing its registers.
    ///
    /// # Safety
    /// The selected bank must be accessible at the current exception level.
    /// The caller must keep this CPU pinned and exclude conflicting accesses,
    /// including timer IRQ and guest entry/exit, for the returned view's life.
    pub unsafe fn current(kind: TimerKind) -> Self {
        Self {
            kind,
            _not_send_sync: PhantomData,
        }
    }

    /// Reads the counter used by this comparator.
    pub fn counter(&self) -> u64 {
        match self.kind {
            TimerKind::Virtual => virtual_counter(),
            TimerKind::Physical | TimerKind::HypervisorPhysical => physical_counter(),
        }
    }

    /// Reads the absolute comparator value, in the selected counter's ticks.
    pub fn compare(&self) -> u64 {
        match self.kind {
            TimerKind::Physical => read_register!("CNTP_CVAL_EL0"),
            TimerKind::Virtual => read_register!("CNTV_CVAL_EL0"),
            TimerKind::HypervisorPhysical => read_register!("CNTHP_CVAL_EL2"),
        }
    }

    /// Writes an absolute comparator value without changing ENABLE or IMASK.
    /// This does not clamp expired deadlines or choose scheduling policy.
    pub fn set_compare(&mut self, ticks: u64) {
        match self.kind {
            TimerKind::Physical => write_register!("CNTP_CVAL_EL0", ticks),
            TimerKind::Virtual => write_register!("CNTV_CVAL_EL0", ticks),
            TimerKind::HypervisorPhysical => write_register!("CNTHP_CVAL_EL2", ticks),
        }
        synchronize();
    }

    /// Reads ENABLE, IMASK and the read-only comparator condition.
    pub fn control(&self) -> TimerControl {
        let value = match self.kind {
            TimerKind::Physical => read_register!("CNTP_CTL_EL0"),
            TimerKind::Virtual => read_register!("CNTV_CTL_EL0"),
            TimerKind::HypervisorPhysical => read_register!("CNTHP_CTL_EL2"),
        };
        TimerControl::from_bits_truncate(value)
    }

    /// Replaces ENABLE and IMASK, ignoring the read-only PENDING flag.
    /// The update is synchronized before returning; the IRQ controller remains
    /// owned by the platform and is neither acknowledged nor reconfigured.
    pub fn set_control(&mut self, control: TimerControl) {
        let value = (control & (TimerControl::ENABLE | TimerControl::MASKED)).bits();
        match self.kind {
            TimerKind::Physical => write_register!("CNTP_CTL_EL0", value),
            TimerKind::Virtual => write_register!("CNTV_CTL_EL0", value),
            TimerKind::HypervisorPhysical => write_register!("CNTHP_CTL_EL2", value),
        }
        synchronize();
    }
}

fn synchronize() {
    // SAFETY: ISB synchronizes architectural state without accessing memory.
    unsafe {
        asm!("isb", options(nostack));
    }
}
