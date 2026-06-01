use ax_errno::{AxResult, ax_err};
use ax_kspin::SpinNoIrq as Mutex;
use axaddrspace::device::{AccessWidth, Port, PortRange};
use axdevice_base::{BaseDeviceOps, EmuDeviceType};

use crate::host;

const PIT_CHANNEL0: u16 = 0x40;
const PIT_CHANNEL2: u16 = 0x42;
const PIT_COMMAND: u16 = 0x43;
const PIT_SPEAKER_CONTROL: u16 = 0x61;
const PIT_PORT_END: u16 = PIT_SPEAKER_CONTROL;

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

    fn write_count(&mut self, value: u8, now_ns: u64) {
        match self.access_mode {
            AccessMode::LatchCount => {}
            AccessMode::LowByte => self.program_reload(value as u16, now_ns),
            AccessMode::HighByte => self.program_reload((value as u16) << 8, now_ns),
            AccessMode::LowThenHigh => {
                if let Some(low) = self.write_low_latched.take() {
                    self.program_reload(((value as u16) << 8) | low as u16, now_ns);
                } else {
                    self.write_low_latched = Some(value);
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
pub struct EmulatedPit {
    state: Mutex<PitState>,
}

impl EmulatedPit {
    /// Create a new PIT device.
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(PitState::new()),
        }
    }

    /// Return whether channel 0 has reached its next IRQ0 deadline.
    ///
    /// When a deadline is reached, this advances the deadline by whole periods so the timer
    /// remains periodic without queueing a burst of missed ticks.
    pub fn consume_irq0_if_due(&self, now_ns: u64) -> bool {
        let mut state = self.state.lock();
        let channel = &mut state.channel0;
        let Some(period_ns) = channel.period_ns else {
            return false;
        };
        if now_ns < channel.next_deadline_ns {
            return false;
        }

        if channel.mode.is_periodic_irq() {
            let elapsed = now_ns.saturating_sub(channel.next_deadline_ns);
            let missed_periods = elapsed / period_ns;
            channel.next_deadline_ns = channel
                .next_deadline_ns
                .saturating_add((missed_periods + 1).saturating_mul(period_ns));
        } else {
            if channel.irq_fired {
                return false;
            }
            channel.irq_fired = true;
        }
        true
    }

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

impl Default for EmulatedPit {
    fn default() -> Self {
        Self::new()
    }
}

impl BaseDeviceOps<PortRange> for EmulatedPit {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::X86Pit
    }

    fn address_range(&self) -> PortRange {
        PortRange::new(Port(PIT_CHANNEL0), Port(PIT_PORT_END))
    }

    fn handle_read(&self, port: Port, width: AccessWidth) -> AxResult<usize> {
        if width != AccessWidth::Byte {
            return ax_err!(Unsupported, "x86 PIT only supports byte port reads");
        }

        let now_ns = host::current_time_nanos();
        let mut state = self.state.lock();
        let value = match port.0 {
            PIT_CHANNEL0 => state.channel0.read_count(now_ns),
            PIT_CHANNEL2 => state.channel2.read_count(now_ns),
            PIT_COMMAND => 0,
            PIT_SPEAKER_CONTROL => {
                let output = state.channel2.output_high(now_ns) as u8;
                (state.speaker_control & !0x20) | (output << 5)
            }
            _ => return ax_err!(Unsupported, "unsupported x86 PIT read port"),
        };
        Ok(value as usize)
    }

    fn handle_write(&self, port: Port, width: AccessWidth, val: usize) -> AxResult {
        if width != AccessWidth::Byte {
            return ax_err!(Unsupported, "x86 PIT only supports byte port writes");
        }

        let now_ns = host::current_time_nanos();
        let mut state = self.state.lock();
        match port.0 {
            PIT_CHANNEL0 => state.channel0.write_count(val as u8, now_ns),
            PIT_CHANNEL2 => state.channel2.write_count(val as u8, now_ns),
            PIT_COMMAND => Self::write_command(&mut state, val as u8, now_ns),
            PIT_SPEAKER_CONTROL => state.speaker_control = val as u8,
            _ => return ax_err!(Unsupported, "unsupported x86 PIT write port"),
        }
        Ok(())
    }
}
