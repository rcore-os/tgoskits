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
    reload_value: u16,
    write_low_latched: Option<u8>,
    read_high_next: bool,
    period_ns: Option<u64>,
    next_deadline_ns: u64,
}

impl PitChannel {
    const fn new() -> Self {
        Self {
            access_mode: AccessMode::LowThenHigh,
            reload_value: 0,
            write_low_latched: None,
            read_high_next: false,
            period_ns: None,
            next_deadline_ns: 0,
        }
    }

    fn program_reload(&mut self, reload_value: u16, now_ns: u64) {
        self.reload_value = reload_value;
        let divisor = if reload_value == 0 {
            0x1_0000
        } else {
            reload_value as u64
        };
        let period_ns =
            ((divisor * NANOSECONDS_PER_SECOND) / PIT_BASE_FREQUENCY_HZ).max(MIN_PERIOD_NS);
        self.period_ns = Some(period_ns);
        self.next_deadline_ns = now_ns.saturating_add(period_ns);
        self.read_high_next = false;
        trace!("x86 PIT channel 0 programmed: reload={reload_value:#x}, period_ns={period_ns}");
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

    fn read_count(&mut self) -> u8 {
        let value = self.reload_value;
        match self.access_mode {
            AccessMode::HighByte => (value >> 8) as u8,
            AccessMode::LowThenHigh => {
                if self.read_high_next {
                    self.read_high_next = false;
                    (value >> 8) as u8
                } else {
                    self.read_high_next = true;
                    value as u8
                }
            }
            AccessMode::LatchCount | AccessMode::LowByte => value as u8,
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

        let elapsed = now_ns.saturating_sub(channel.next_deadline_ns);
        let missed_periods = elapsed / period_ns;
        channel.next_deadline_ns = channel
            .next_deadline_ns
            .saturating_add((missed_periods + 1).saturating_mul(period_ns));
        true
    }

    fn write_command(state: &mut PitState, command: u8) {
        let channel = (command >> 6) & 0b11;
        let access_mode = AccessMode::from_command(command);
        if access_mode == AccessMode::LatchCount {
            return;
        }

        match channel {
            0 => {
                state.channel0.access_mode = access_mode;
                state.channel0.write_low_latched = None;
                state.channel0.read_high_next = false;
            }
            2 => {
                state.channel2.access_mode = access_mode;
                state.channel2.write_low_latched = None;
                state.channel2.read_high_next = false;
            }
            _ => debug!("x86 PIT command for unsupported channel {channel}: {command:#x}"),
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

        let mut state = self.state.lock();
        let value = match port.0 {
            PIT_CHANNEL0 => state.channel0.read_count(),
            PIT_CHANNEL2 => state.channel2.read_count(),
            PIT_COMMAND => 0,
            PIT_SPEAKER_CONTROL => state.speaker_control,
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
            PIT_COMMAND => Self::write_command(&mut state, val as u8),
            PIT_SPEAKER_CONTROL => state.speaker_control = val as u8,
            _ => return ax_err!(Unsupported, "unsupported x86 PIT write port"),
        }
        Ok(())
    }
}
