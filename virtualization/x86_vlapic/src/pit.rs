use alloc::sync::Arc;

use ax_sync::Mutex;

use crate::{
    host::*,
    timer_registration::{
        TimerRegistration, limit_periodic_timer_period_ns, restart_periodic_deadline_ns,
    },
    *,
};

const PIT_CHANNEL0: u16 = 0x40;
const PIT_CHANNEL2: u16 = 0x42;
const PIT_COMMAND: u16 = 0x43;
const PIT_SPEAKER_CONTROL: u16 = 0x61;

const PIT_BASE_FREQUENCY_HZ: u64 = 1_193_182;
const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;
const MIN_PERIOD_NS: u64 = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AccessMode {
    LatchCount,
    LowByte,
    HighByte,
    LowThenHigh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PitMode {
    InterruptOnTerminalCount,
    HardwareRetriggerableOneShot,
    RateGenerator,
    SquareWaveGenerator,
    SoftwareTriggeredStrobe,
    HardwareTriggeredStrobe,
}

impl PitMode {
    fn from_command(command: u8) -> Self {
        match (command >> 1) & 0b111 {
            0 => Self::InterruptOnTerminalCount,
            1 => Self::HardwareRetriggerableOneShot,
            2 | 6 => Self::RateGenerator,
            3 | 7 => Self::SquareWaveGenerator,
            4 => Self::SoftwareTriggeredStrobe,
            _ => Self::HardwareTriggeredStrobe,
        }
    }

    const fn raw_bits(self) -> u8 {
        match self {
            Self::InterruptOnTerminalCount => 0,
            Self::HardwareRetriggerableOneShot => 1,
            Self::RateGenerator => 2,
            Self::SquareWaveGenerator => 3,
            Self::SoftwareTriggeredStrobe => 4,
            Self::HardwareTriggeredStrobe => 5,
        }
    }

    const fn is_periodic_irq(self) -> bool {
        matches!(self, Self::RateGenerator | Self::SquareWaveGenerator)
    }
}

impl AccessMode {
    fn from_command(command: u8) -> Self {
        match (command >> 4) & 0b11 {
            0 => Self::LatchCount,
            1 => Self::LowByte,
            2 => Self::HighByte,
            _ => Self::LowThenHigh,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct PitChannel {
    access_mode: AccessMode,
    mode: PitMode,
    reload_value: u16,
    write_low_latched: Option<u8>,
    read_high_next: bool,
    latched_count: Option<u16>,
    latched_status: Option<u8>,
    null_count: bool,
    start_ns: u64,
    period_ns: Option<u64>,
    next_deadline_ns: u64,
    irq_fired: bool,
}

impl PitChannel {
    const fn new() -> Self {
        Self {
            access_mode: AccessMode::LowThenHigh,
            mode: PitMode::SquareWaveGenerator,
            reload_value: 0,
            write_low_latched: None,
            read_high_next: false,
            latched_count: None,
            latched_status: None,
            null_count: true,
            start_ns: 0,
            period_ns: None,
            next_deadline_ns: 0,
            irq_fired: false,
        }
    }

    fn divisor(&self) -> u64 {
        if self.reload_value == 0 {
            0x1_0000
        } else {
            self.reload_value as u64
        }
    }

    fn program_reload(&mut self, reload_value: u16, now_ns: u64) {
        self.reload_value = reload_value;
        let divisor = self.divisor();
        let period_ns =
            ((divisor * NANOSECONDS_PER_SECOND) / PIT_BASE_FREQUENCY_HZ).max(MIN_PERIOD_NS);
        self.start_ns = now_ns;
        self.period_ns = Some(period_ns);
        self.next_deadline_ns = now_ns.saturating_add(period_ns);
        self.read_high_next = false;
        self.latched_count = None;
        self.latched_status = None;
        self.null_count = false;
        self.irq_fired = false;
    }

    fn write_count(&mut self, value: u8, now_ns: u64) -> bool {
        match self.access_mode {
            AccessMode::LatchCount => false,
            AccessMode::LowByte => {
                self.program_reload(value as u16, now_ns);
                true
            }
            AccessMode::HighByte => {
                self.program_reload((value as u16) << 8, now_ns);
                true
            }
            AccessMode::LowThenHigh => {
                if let Some(low) = self.write_low_latched.take() {
                    self.program_reload(((value as u16) << 8) | low as u16, now_ns);
                    true
                } else {
                    self.write_low_latched = Some(value);
                    false
                }
            }
        }
    }

    fn elapsed_ticks(&self, now_ns: u64) -> u64 {
        let elapsed_ns = now_ns.saturating_sub(self.start_ns);
        elapsed_ns.saturating_mul(PIT_BASE_FREQUENCY_HZ) / NANOSECONDS_PER_SECOND
    }

    fn current_count(&self, now_ns: u64) -> u16 {
        let Some(_) = self.period_ns else {
            return self.reload_value;
        };
        let divisor = self.divisor();
        let elapsed_ticks = self.elapsed_ticks(now_ns);

        if !self.mode.is_periodic_irq() && elapsed_ticks >= divisor {
            return 0;
        }

        let remaining = divisor - (elapsed_ticks % divisor);
        if remaining == 0x1_0000 {
            0
        } else {
            remaining as u16
        }
    }

    fn output_high(&self, now_ns: u64) -> bool {
        let Some(_) = self.period_ns else {
            return true;
        };
        let divisor = self.divisor();
        let elapsed_ticks = self.elapsed_ticks(now_ns);
        match self.mode {
            PitMode::InterruptOnTerminalCount | PitMode::SoftwareTriggeredStrobe => {
                elapsed_ticks >= divisor
            }
            PitMode::RateGenerator => elapsed_ticks % divisor != divisor.saturating_sub(1),
            PitMode::SquareWaveGenerator => (elapsed_ticks % divisor) < divisor.div_ceil(2),
            PitMode::HardwareRetriggerableOneShot | PitMode::HardwareTriggeredStrobe => true,
        }
    }

    fn latch_status(&mut self, now_ns: u64) {
        if self.latched_status.is_none() {
            let mut status = (self.output_high(now_ns) as u8) << 7;
            status |= (self.null_count as u8) << 6;
            status |= match self.access_mode {
                AccessMode::LatchCount => 0,
                AccessMode::LowByte => 1,
                AccessMode::HighByte => 2,
                AccessMode::LowThenHigh => 3,
            } << 4;
            status |= self.mode.raw_bits() << 1;
            self.latched_status = Some(status);
        }
    }

    fn latch_count(&mut self, now_ns: u64) {
        if self.latched_count.is_none() {
            self.latched_count = Some(self.current_count(now_ns));
            self.read_high_next = false;
        }
    }

    fn read_count(&mut self, now_ns: u64) -> u8 {
        if let Some(status) = self.latched_status.take() {
            return status;
        }

        let value = self
            .latched_count
            .unwrap_or_else(|| self.current_count(now_ns));
        match self.access_mode {
            AccessMode::HighByte => {
                self.latched_count = None;
                (value >> 8) as u8
            }
            AccessMode::LowThenHigh => {
                if self.read_high_next {
                    self.read_high_next = false;
                    self.latched_count = None;
                    (value >> 8) as u8
                } else {
                    self.read_high_next = true;
                    value as u8
                }
            }
            AccessMode::LatchCount | AccessMode::LowByte => {
                self.latched_count = None;
                value as u8
            }
        }
    }
}

#[derive(Debug)]
struct PitState {
    channel0: PitChannel,
    channel2: PitChannel,
    speaker_control: u8,
}

impl PitState {
    const fn new() -> Self {
        Self {
            channel0: PitChannel::new(),
            channel2: PitChannel::new(),
            speaker_control: 0,
        }
    }
}

/// A minimal emulated x86 PIT/8254 device.
pub struct EmulatedPit<H: X86VlapicHostOps> {
    state: Mutex<PitState>,
    /// Task-side serialization of the IRQ0 host timer.
    ///
    /// Registering or cancelling a host timer runs external host code that may
    /// allocate or wait, so this uses a sleepable mutex instead of a raw lock.
    /// The hard-timer callback never takes this mutex; it only owns the short
    /// IRQ-safe arm state inside [`TimerRegistration`].
    irq0_timer: Mutex<PitIrqTimer<H::Runtime>>,
}

struct PitIrqTimer<R: X86VlapicRuntimeOps> {
    registration: Arc<TimerRegistration<R>>,
    runtime: R,
}

impl<R: X86VlapicRuntimeOps> PitIrqTimer<R> {
    fn new(runtime: R) -> Self {
        Self {
            registration: Arc::new(TimerRegistration::new()),
            runtime,
        }
    }
}

impl<H: X86VlapicHostOps> EmulatedPit<H> {
    /// Create a new PIT device.
    pub fn new() -> Self {
        Self::new_for_vcpu(0, 0)
    }

    /// Create a PIT whose IRQ0 uses the supplied run-scoped port.
    pub fn new_for_vcpu_with_runtime(
        runtime: H::Runtime,
        _vm_id: X86VmId,
        _vcpu_id: X86VcpuId,
    ) -> Self {
        Self {
            state: Mutex::new(PitState::new()),
            irq0_timer: Mutex::new(PitIrqTimer::new(runtime)),
        }
    }

    /// Create a host-side adapter that is not attached to a guest run.
    ///
    /// Programming its timer returns a run-state error. AxVM creates its real
    /// PIT with [`Self::new_for_vcpu_with_runtime`] from the run owner.
    pub fn new_for_vcpu(vm_id: X86VmId, vcpu_id: X86VcpuId) -> Self {
        Self::new_for_vcpu_with_runtime(H::unbound_runtime(vm_id, vcpu_id), vm_id, vcpu_id)
    }
}

impl<H: X86VlapicHostOps> Default for EmulatedPit<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H: X86VlapicHostOps> EmulatedPit<H> {
    fn channel_mut(state: &mut PitState, channel: u8) -> Option<&mut PitChannel> {
        match channel {
            0 => Some(&mut state.channel0),
            2 => Some(&mut state.channel2),
            _ => None,
        }
    }

    fn write_command(state: &mut PitState, command: u8, now_ns: u64) {
        let channel = (command >> 6) & 0b11;
        if channel == 0b11 {
            Self::write_read_back_command(state, command, now_ns);
            return;
        }

        let access_mode = AccessMode::from_command(command);
        let mode = PitMode::from_command(command);
        let Some(pit_channel) = Self::channel_mut(state, channel) else {
            debug!("x86 PIT command for unsupported channel {channel}: {command:#x}");
            return;
        };

        if access_mode == AccessMode::LatchCount {
            pit_channel.latch_count(now_ns);
            return;
        }

        pit_channel.access_mode = access_mode;
        pit_channel.mode = mode;
        pit_channel.write_low_latched = None;
        pit_channel.read_high_next = false;
        pit_channel.latched_count = None;
        pit_channel.latched_status = None;
        pit_channel.null_count = true;
    }

    fn write_read_back_command(state: &mut PitState, command: u8, now_ns: u64) {
        let latch_count = command & (1 << 5) == 0;
        let latch_status = command & (1 << 4) == 0;
        let selected = command & 0b1110;

        if selected & (1 << 1) != 0 {
            if latch_count {
                state.channel0.latch_count(now_ns);
            }
            if latch_status {
                state.channel0.latch_status(now_ns);
            }
        }
        if selected & (1 << 3) != 0 {
            if latch_count {
                state.channel2.latch_count(now_ns);
            }
            if latch_status {
                state.channel2.latch_status(now_ns);
            }
        }
    }
}

impl<R: X86VlapicRuntimeOps> PitIrqTimer<R> {
    fn schedule(&mut self, deadline_ns: u64, period_ns: Option<u64>) -> X86VlapicResult {
        self.registration.invalidate_and_cancel(&self.runtime)?;
        schedule_irq0(
            deadline_ns,
            period_ns,
            Arc::clone(&self.registration),
            self.runtime.clone(),
        )
    }

    fn cancel(&mut self) -> X86VlapicResult {
        self.registration.invalidate_and_cancel(&self.runtime)
    }
}

impl<R: X86VlapicRuntimeOps> Drop for PitIrqTimer<R> {
    fn drop(&mut self) {
        if let Err(error) = self.cancel() {
            log::warn!("failed to cancel x86 PIT timer during teardown: {error:?}");
        }
    }
}

fn schedule_irq0<R: X86VlapicRuntimeOps>(
    deadline_ns: u64,
    period_ns: Option<u64>,
    registration: Arc<TimerRegistration<R>>,
    runtime: R,
) -> X86VlapicResult {
    let mut next_deadline_ns = deadline_ns;
    registration.register(
        &runtime,
        deadline_ns,
        alloc::boxed::Box::new(move |now_ns| {
            let _ = runtime.inject_pit_irq();
            if let Some(period_ns) = period_ns {
                next_deadline_ns =
                    restart_periodic_deadline_ns(next_deadline_ns, period_ns, now_ns);
                return X86TimerAction::Rearm(next_deadline_ns);
            }
            X86TimerAction::Complete
        }),
    )
}

impl<H: X86VlapicHostOps> EmulatedPit<H> {
    /// Returns the two disjoint PIT port ranges.
    pub const fn port_ranges() -> [X86PortRange; 2] {
        [
            X86PortRange::new(X86Port::new(PIT_CHANNEL0), X86Port::new(PIT_COMMAND)),
            X86PortRange::new(
                X86Port::new(PIT_SPEAKER_CONTROL),
                X86Port::new(PIT_SPEAKER_CONTROL),
            ),
        ]
    }

    /// Handles a PIT port read.
    pub fn handle_read(&self, port: X86Port, width: X86AccessWidth) -> X86VlapicResult<usize> {
        if width != X86AccessWidth::Byte {
            return Err(X86VlapicError::Unsupported);
        }

        let now_ns = host::current_time_nanos::<H>();
        let mut state = self.state.lock();
        let value = match port.number() {
            PIT_CHANNEL0 => state.channel0.read_count(now_ns),
            PIT_CHANNEL2 => state.channel2.read_count(now_ns),
            PIT_COMMAND => 0,
            PIT_SPEAKER_CONTROL => {
                let output = state.channel2.output_high(now_ns) as u8;
                (state.speaker_control & !0x20) | (output << 5)
            }
            _ => return Err(X86VlapicError::Unsupported),
        };
        Ok(value as usize)
    }

    /// Handles a PIT port write.
    pub fn handle_write(
        &self,
        port: X86Port,
        width: X86AccessWidth,
        val: usize,
    ) -> X86VlapicResult {
        if width != X86AccessWidth::Byte {
            return Err(X86VlapicError::Unsupported);
        }

        let now_ns = host::current_time_nanos::<H>();
        let mut state = self.state.lock();
        let irq0_schedule = match port.number() {
            PIT_CHANNEL0 if state.channel0.write_count(val as u8, now_ns) => {
                let period_ns = state.channel0.period_ns;
                let repeat_ns = state
                    .channel0
                    .mode
                    .is_periodic_irq()
                    .then_some(period_ns)
                    .flatten()
                    .map(limit_periodic_timer_period_ns);
                Some((state.channel0.next_deadline_ns, repeat_ns))
            }
            PIT_CHANNEL0 => None,
            PIT_CHANNEL2 => {
                state.channel2.write_count(val as u8, now_ns);
                None
            }
            PIT_COMMAND => {
                Self::write_command(&mut state, val as u8, now_ns);
                None
            }
            PIT_SPEAKER_CONTROL => {
                state.speaker_control = val as u8;
                None
            }
            _ => return Err(X86VlapicError::Unsupported),
        };
        drop(state);
        if let Some((deadline_ns, period_ns)) = irq0_schedule {
            // Task-side serialization only: this sleepable guard is held across
            // the host timer register/cancel, which may allocate or wait. The
            // hard-timer callback retires the previous arm through
            // `TimerRegistration`'s short IRQ-safe state and never takes it.
            let mut timer = self.irq0_timer.lock();
            timer.schedule(deadline_ns, period_ns)?;
        }
        Ok(())
    }
}
