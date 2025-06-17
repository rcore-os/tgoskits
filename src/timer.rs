use axerrno::AxResult;

/// Local APIC timer modes.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
#[allow(dead_code)]
pub enum TimerMode {
    /// Timer only fires once.
    OneShot = 0b00,
    /// Timer fires periodically.
    Periodic = 0b01,
    /// Timer fires at an absolute time.
    TscDeadline = 0b10,
}

/// A virtual local APIC timer. (SDM Vol. 3C, Section 11.5.4)
pub struct ApicTimer {
    lvt_timer_bits: u32,
    divide_shift: u8,
    initial_count: u32,
    last_start_cycle: u64,
    deadline_ns: u64,
    timer_mode: TimerMode,
}

impl ApicTimer {
    pub(crate) const fn new() -> Self {
        Self {
            lvt_timer_bits: 0x1_0000, // masked
            divide_shift: 0,
            initial_count: 0,
            last_start_cycle: 0,
            deadline_ns: 0,
            timer_mode: TimerMode::OneShot,
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

    /// Gets the timer mode.
    pub fn timer_mode(&self) -> TimerMode {
        self.timer_mode
    }

    /// Update the timer mode if it is different from the current mode.
    pub fn update_timer_mode(&mut self, mode: TimerMode) -> AxResult {
        if self.timer_mode != mode {
            self.timer_mode = mode;
            unimplemented!("del timer and update timer")
        }

        Ok(())
    }

    pub fn start_timer(&mut self) -> AxResult {
        unimplemented!("start timer")
    }

    pub fn delete_timer(&mut self) -> AxResult {
        self.initial_count = 0;
        self.deadline_ns = 0;
        unimplemented!("del timer")
    }

    /// Whether the timer interrupt is masked.
    pub const fn is_masked(&self) -> bool {
        self.lvt_timer_bits & (1 << 16) != 0
    }

    /// Whether the timer mode is periodic.
    pub const fn is_periodic(&self) -> bool {
        let timer_mode = (self.lvt_timer_bits >> 17) & 0b11;
        timer_mode == TimerMode::Periodic as _
    }

    /// The timer interrupt vector number.
    pub const fn vector(&self) -> u8 {
        (self.lvt_timer_bits & 0xff) as u8
    }

    /// LVT Timer Register. (SDM Vol. 3A, Section 10.5.1, Figure 10-8)
    pub const fn lvt_timer(&self) -> u32 {
        self.lvt_timer_bits
    }

    /// Divide Configuration Register. (SDM Vol. 3A, Section 10.5.4, Figure 10-10)
    pub const fn divide(&self) -> u32 {
        let dcr = self.divide_shift.wrapping_sub(1) as u32 & 0b111;
        (dcr & 0b11) | ((dcr & 0b100) << 1)
    }

    /// Initial Count Register.
    pub const fn initial_count(&self) -> u32 {
        self.initial_count
    }

    /// Current Count Register.
    pub fn current_counter(&self) -> u32 {
        if self.initial_count == 0 || self.timer_mode == TimerMode::TscDeadline {
            return 0;
        }

        // Todo: get current cycles
        let current_cycles = 0;

        let elapsed_cycles = (current_cycles - self.last_start_cycle) >> self.divide_shift;
        match self.timer_mode {
            TimerMode::OneShot => self.initial_count - elapsed_cycles as u32,
            TimerMode::Periodic => {
                self.initial_count - (elapsed_cycles % self.initial_count as u64) as u32
            }
            TimerMode::TscDeadline => 0,
        }
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
