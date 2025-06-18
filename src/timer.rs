use alloc::boxed::Box;
use axerrno::{AxResult, ax_err};
use axvisor_api::{
    time::{self, current_ticks, register_timer, ticks_to_nanos, ticks_to_time},
    vmm::{VCpuId, VMId, inject_interrupt},
};

use crate::{
    consts::RESET_LVT_REG,
    regs::lvt::{
        LVT_TIMER::{self, TimerMode::Value as TimerMode},
        LvtTimerRegisterLocal,
    },
};

/// A virtual local APIC timer. (SDM Vol. 3C, Section 11.5.4)
///
/// This struct virtualizes the access to 4 registers in the Local APIC:
///
/// - LVT Timer Register. (SDM Vol. 3A, Section 11.5.1, Figure 11-8, offset 0x320, MSR 0x832, Read/Write)
/// - Divide Configuration Register. (SDM Vol. 3A, Section 11.5.4, Figure 11-10, offset 0x3E0, MSR 0x83E, Read/Write)
/// - Initial Count Register. (SDM Vol. 3A, Section 11.5.4, Figure 11-11, offset 0x380, MSR 0x838, Read/Write)
/// - Current Count Register. (SDM Vol. 3A, Section 11.5.4, Figure 11-11, offset 0x390, MSR 0x839, Read Only)
///
/// The timer works in the following way:
///
/// - Timer is started by and only by writing to the Initial Count Register.
/// - The deadline is determined by the Initial Count Register and the Divide Configuration Register, at the time of the start.
/// - Any modification to the Divide Configuration Register or the LVT Timer Register will not affect the current timer.
/// - Any write to the Initial Count Register will restart the timer.
/// - The value of the LVT Timer is read, at the time the deadline is reached, to determine
///   - if an interrupt should be generated (not masked),
///   - if the timer should be restarted (periodic mode), and
///   - the interrupt vector number to be used.
/// - The delivery status field in the LVT Timer Register is not supported and always returns 0.
/// - The timer stops when:
///   - the deadline is reached, and the timer is in one-shot mode, or
///   - a 0 is written to the Initial Count Register.
pub struct ApicTimer {
    // the raw value of writable registers
    /// Local Vector Table Timer Register. These's another copy in [`VirtualApicRegs`](crate::VirtualApicRegs), but we
    /// keep a separate copy here for easier access.
    lvt_timer_register: LvtTimerRegisterLocal,
    /// Initial Count Register. This is the value that determines when the timer will fire.
    initial_count_register: u32,
    /// Divide Configuration Register. This determines the frequency of the timer.
    divide_configuration_register: u32,

    // internal states
    divide_shift: u8,
    last_start_ticks: u64,
    deadline_ns: u64,

    // temporary fields untils we find a permanent place for apic and its timer
    cancel_token: Option<usize>,
    where_am_i: (VMId, VCpuId), // (vm_id, vcpu_id)
}

impl ApicTimer {
    pub(crate) const fn new(vm_id: VMId, vcpu_id: VCpuId) -> Self {
        Self {
            lvt_timer_register: LvtTimerRegisterLocal::new(RESET_LVT_REG), // masked, one-shot, vector 0
            initial_count_register: 0,                                     // 0 (stopped)
            divide_configuration_register: 0,                              // divide by 2

            divide_shift: 1, // as `divide_configuration_register` is 0, the shift is 1 (divide by 2)
            last_start_ticks: 0,
            deadline_ns: 0,
            cancel_token: None,
            where_am_i: (vm_id, vcpu_id),
        }
    }

    // /// Check if an interrupt generated. if yes, update it's states.
    // pub fn check_interrupt(&mut self) -> bool {
    //     if self.deadline_ns == 0 {
    //         false
    //     } else if H::current_time_nanos() >= self.deadline_ns {
    //         if self.is_periodic() {
    //             self.deadline_ns += self.interval_ns();
    //         } else {
    //             self.deadline_ns = 0;
    //         }
    //         !self.is_masked()
    //     } else {
    //         false
    //     }
    // }

    pub fn read_lvt(&self) -> u32 {
        self.lvt_timer_register.get()
    }

    pub fn write_lvt(&mut self, mut value: u32) -> AxResult {
        // valid bits: 0-7, 12, 16-18
        const LVT_MASK: u32 = 0x0007_10FF;

        value &= LVT_MASK;
        self.lvt_timer_register.set(value);
        Ok(())
    }

    pub fn read_icr(&self) -> u32 {
        self.initial_count_register
    }

    pub fn write_icr(&mut self, value: u32) -> AxResult {
        // stop the timer no matter whether it is started, and no matter the value
        self.stop_timer()?;
        self.initial_count_register = value;

        if value > 0 {
            self.start_timer()
        } else {
            Ok(())
        }
    }

    /// Read from the Divide Configuration Register.
    pub fn read_dcr(&self) -> u32 {
        self.divide_configuration_register
    }

    /// Write to the Divide Configuration Register.
    pub fn write_dcr(&mut self, mut value: u32) {
        const DCR_MASK: u32 = 0b1011;

        value &= DCR_MASK;
        let shift = match value {
            0b0000 => 1, // divide by 2
            0b0001 => 2, // divide by 4
            0b0010 => 3, // divide by 8
            0b0011 => 4, // divide by 16
            0b1000 => 5, // divide by 32
            0b1001 => 6, // divide by 64
            0b1010 => 7, // divide by 128
            0b1011 => 0, // divide by 1
            _ => unreachable!(
                "internal error: invalid divide configuration register value after mask"
            ),
        };

        self.divide_configuration_register = value;
        self.divide_shift = shift as u8;
    }

    /// Current Count Register.
    pub fn read_ccr(&self) -> u32 {
        if !self.is_started() {
            return 0;
        }
        let remaining_ns = self.deadline_ns.wrapping_sub(time::current_time_nanos());
        let remaining_ticks = time::nanos_to_ticks(remaining_ns);
        return (remaining_ticks >> self.divide_shift) as _;
    }

    /// Get the timer mode.
    pub fn timer_mode(&self) -> TimerMode {
        self.lvt_timer_register
            .read_as_enum(LVT_TIMER::TimerMode)
            .unwrap() // just panic if the value is invalid
    }

    /// Check whether the timer interrupt is masked.
    pub fn is_masked(&self) -> bool {
        self.lvt_timer_register.is_set(LVT_TIMER::Mask)
    }

    /// The timer interrupt vector number.
    pub fn vector(&self) -> u8 {
        self.lvt_timer_register.read(LVT_TIMER::Vector) as u8
    }

    /// Check whether the timer is started.
    pub fn is_started(&self) -> bool {
        // these two conditions are equivalent actually, we check both for clarity and robustness
        self.initial_count_register > 0 && self.cancel_token.is_some()
    }

    /// Restart the timer. Will not start the timer if it is not started.
    pub fn restart_timer(&mut self) -> AxResult {
        if !self.is_started() {
            return Ok(());
        } else {
            self.stop_timer()?;
            self.start_timer()
        }
    }

    /// Start the timer.
    pub fn start_timer(&mut self) -> AxResult {
        if self.is_started() {
            return ax_err!(BadState, "Timer already started");
        }

        let current_ticks = current_ticks();
        let deadline_ticks =
            current_ticks + ((self.initial_count_register as u64) << self.divide_shift);
        let (vm_id, vcpu_id) = self.where_am_i;
        let vector = self.vector();

        trace!(
            "vlapic @ (vm {}, vcpu {}) starts timer @ tick {:?}, deadline tick {:?}",
            vm_id, vcpu_id, current_ticks, deadline_ticks
        );

        self.last_start_ticks = current_ticks;
        self.deadline_ns = ticks_to_nanos(deadline_ticks);

        self.cancel_token = Some(register_timer(
            ticks_to_time(deadline_ticks),
            Box::new(move |_| {
                // TODO: read the LVT Timer Register here
                trace!(
                    "vlapic @ (vm {}, vcpu {}) timer expired, inject interrupt {}",
                    vm_id, vcpu_id, vector
                );
                inject_interrupt(vm_id, vcpu_id, vector);
            }),
        ));

        Ok(())
    }

    pub fn stop_timer(&mut self) -> AxResult {
        // TODO: maybe disable irq here?
        if self.is_started() {
            self.last_start_ticks = 0;
            self.deadline_ns = 0;

            time::cancel_timer(self.cancel_token.take().unwrap());
        } else {
            warn!("`stop_timer` called when timer is not started, bad operation tolerated");
        }

        Ok(())
    }

    /// Whether the timer mode is periodic.
    pub fn is_periodic(&self) -> bool {
        self.timer_mode() == TimerMode::Periodic
    }

    // /// Set LVT Timer Register.
    // pub fn set_lvt_timer(&mut self, bits: u32) -> RvmResult {
    //     let timer_mode = bits.get_bits(17..19);
    //     if timer_mode == TimerMode::TscDeadline as _ {
    //         return rvm_err!(Unsupported); // TSC deadline mode was not supported
    //     } else if timer_mode == 0b11 {
    //         return rvm_err!(InvalidParam); // reserved
    //     }
    //     self.lvt_timer_bits = bits;
    //     self.start_timer();
    //     Ok(())
    // }

    // /// Set Initial Count Register.
    // pub fn set_initial_count(&mut self, initial: u32) -> RvmResult {
    //     self.initial_count = initial;
    //     self.start_timer();
    //     Ok(())
    // }

    // /// Set Divide Configuration Register.
    // pub fn set_divide(&mut self, dcr: u32) -> RvmResult {
    //     let shift = (dcr & 0b11) | ((dcr & 0b1000) >> 1);
    //     self.divide_shift = (shift + 1) as u8 & 0b111;
    //     self.start_timer();
    //     Ok(())
    // }

    // const fn interval_ns(&self) -> u64 {
    //     (self.initial_count as u64 * APIC_CYCLE_NANOS) << self.divide_shift
    // }

    // fn start_timer(&mut self) {
    //     if self.initial_count != 0 {
    //         self.last_start_cycle = H::current_time_nanos();
    //         self.deadline_ns = self.last_start_cycle + self.interval_ns();
    //     } else {
    //         self.deadline_ns = 0;
    //     }
    // }
}
