use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

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

/// Owner-local 8254 register file.
///
/// The core contains no synchronization or host timer handle.  A shared
/// interrupt owner applies register transactions through `&mut self` and
/// then schedules the returned timer plan after the transaction completes.
pub struct PitCore {
    state: PitState,
}

/// Host timer operation requested by a [`PitCore`] register write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PitTimerPlan {
    /// Absolute host deadline for the first IRQ0 edge.
    pub deadline_ns: u64,
    /// Period for a rate/square-wave channel, or `None` for one-shot modes.
    pub period_ns: Option<u64>,
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

fn write_command(state: &mut PitState, command: u8, now_ns: u64) {
    let channel = (command >> 6) & 0b11;
    if channel == 0b11 {
        write_read_back_command(state, command, now_ns);
        return;
    }

    let access_mode = AccessMode::from_command(command);
    let mode = PitMode::from_command(command);
    let Some(pit_channel) = (match channel {
        0 => Some(&mut state.channel0),
        2 => Some(&mut state.channel2),
        _ => None,
    }) else {
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

impl PitCore {
    /// Creates a reset-compatible register file.
    pub const fn new() -> Self {
        Self {
            state: PitState::new(),
        }
    }

    /// Handles one PIT read without taking a lock or touching host timers.
    pub fn handle_read(
        &mut self,
        port: X86Port,
        width: X86AccessWidth,
        now_ns: u64,
    ) -> X86VlapicResult<usize> {
        if width != X86AccessWidth::Byte {
            return Err(X86VlapicError::Unsupported);
        }
        let value = match port.number() {
            PIT_CHANNEL0 => self.state.channel0.read_count(now_ns),
            PIT_CHANNEL2 => self.state.channel2.read_count(now_ns),
            PIT_COMMAND => 0,
            PIT_SPEAKER_CONTROL => {
                let output = self.state.channel2.output_high(now_ns) as u8;
                (self.state.speaker_control & !0x20) | (output << 5)
            }
            _ => return Err(X86VlapicError::Unsupported),
        };
        Ok(value as usize)
    }

    /// Handles one PIT write and returns a host timer plan when channel 0 was
    /// fully reprogrammed.
    pub fn handle_write(
        &mut self,
        port: X86Port,
        width: X86AccessWidth,
        val: usize,
        now_ns: u64,
    ) -> X86VlapicResult<Option<PitTimerPlan>> {
        if width != X86AccessWidth::Byte {
            return Err(X86VlapicError::Unsupported);
        }
        let timer_plan = match port.number() {
            PIT_CHANNEL0 if self.state.channel0.write_count(val as u8, now_ns) => {
                let channel = &self.state.channel0;
                let repeat_ns = channel
                    .mode
                    .is_periodic_irq()
                    .then_some(channel.period_ns)
                    .flatten()
                    .map(limit_periodic_timer_period_ns);
                Some(PitTimerPlan {
                    deadline_ns: channel.next_deadline_ns,
                    period_ns: repeat_ns,
                })
            }
            PIT_CHANNEL0 => None,
            PIT_CHANNEL2 => {
                self.state.channel2.write_count(val as u8, now_ns);
                None
            }
            PIT_COMMAND => {
                write_command(&mut self.state, val as u8, now_ns);
                None
            }
            PIT_SPEAKER_CONTROL => {
                self.state.speaker_control = val as u8;
                None
            }
            _ => return Err(X86VlapicError::Unsupported),
        };
        Ok(timer_plan)
    }

    /// Resets the guest-visible register file.
    pub fn reset(&mut self) {
        self.state = PitState::new();
    }
}

impl Default for PitCore {
    fn default() -> Self {
        Self::new()
    }
}

/// Task-owned PIT service: the 8254 register file and its IRQ0 host timer.
///
/// A guest programming write updates the register file and then (re)registers
/// the host timer. Both live under one sleepable mutex so two concurrent writes
/// cannot interleave and leave a stale timer armed after a newer reload, and so
/// suspend/resume/stop observe the register file and the registration together.
/// The hard-timer callback never takes this mutex; it only owns the short
/// IRQ-safe arm state inside [`TimerRegistration`].
struct PitService<R: X86VlapicRuntimeOps> {
    core: PitCore,
    timer: PitIrqTimer<R>,
    /// Set while a VM suspend quiesced a live arm that still owes a resume.
    suspended: bool,
}

/// A minimal emulated x86 PIT/8254 device.
pub struct EmulatedPit<H: X86VlapicHostOps> {
    service: Mutex<PitService<H::Runtime>>,
}

struct PitIrqTimer<R: X86VlapicRuntimeOps> {
    registration: Arc<TimerRegistration<R>>,
    runtime: R,
    /// Canonical absolute deadline of the armed IRQ0 host timer. The callback
    /// publishes each periodic rearm and clears it when a one-shot completes,
    /// so resume can reinstall the exact countdown instead of the stale
    /// program-time reload deadline.
    deadline_ns: Arc<AtomicU64>,
}

impl<R: X86VlapicRuntimeOps + Clone> PitIrqTimer<R> {
    fn new(runtime: R) -> Self {
        Self {
            registration: Arc::new(TimerRegistration::new()),
            runtime,
            deadline_ns: Arc::new(AtomicU64::new(0)),
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
            service: Mutex::new(PitService {
                core: PitCore::new(),
                timer: PitIrqTimer::new(runtime),
                suspended: false,
            }),
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

impl<R: X86VlapicRuntimeOps + Clone> PitIrqTimer<R> {
    fn schedule(&mut self, deadline_ns: u64, period_ns: Option<u64>) -> X86VlapicResult {
        // Retire any prior arm through the full host cancel barrier first,
        // including a callback that already completed but whose stable handle
        // has not been reclaimed yet.
        self.registration.invalidate_and_cancel(&self.runtime)?;
        // Publish the initial deadline before arming so a callback that fires
        // during registration can only overwrite it with a newer rearm.
        self.deadline_ns.store(deadline_ns, Ordering::Release);
        schedule_irq0(
            deadline_ns,
            period_ns,
            Arc::clone(&self.registration),
            self.runtime.clone(),
            Arc::clone(&self.deadline_ns),
        )
    }

    fn cancel(&mut self) -> X86VlapicResult {
        self.registration
            .invalidate_and_cancel(&self.runtime)
            .map(|_| ())
    }
}

impl<R: X86VlapicRuntimeOps> Drop for PitIrqTimer<R> {
    fn drop(&mut self) {
        // The task-side lifecycle must quiesce this producer before the PIT is
        // released: `suspend`/`stop` return cancellation failures so the caller
        // retries. Reaching drop with a live arm is a contract violation that
        // must not be hidden behind a warn-and-drop. A completed one-shot may
        // still carry a retained (retired) handle and is not a live producer.
        assert!(
            !self.registration.is_active(),
            "x86 PIT timer dropped while its host registration was live",
        );
    }
}

fn schedule_irq0<R: X86VlapicRuntimeOps + Clone>(
    deadline_ns: u64,
    period_ns: Option<u64>,
    registration: Arc<TimerRegistration<R>>,
    runtime: R,
    canonical_deadline_ns: Arc<AtomicU64>,
) -> X86VlapicResult {
    let mut next_deadline_ns = deadline_ns;
    let callback_runtime = runtime.clone();
    registration.register(
        &runtime,
        deadline_ns,
        alloc::boxed::Box::new(move |now_ns| {
            let _ = callback_runtime.inject_pit_irq();
            if let Some(period_ns) = period_ns {
                next_deadline_ns =
                    restart_periodic_deadline_ns(next_deadline_ns, period_ns, now_ns);
                canonical_deadline_ns.store(next_deadline_ns, Ordering::Release);
                return X86TimerAction::Rearm(next_deadline_ns);
            }
            canonical_deadline_ns.store(0, Ordering::Release);
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
        let mut service = self.service.lock();
        service.core.handle_read(port, width, now_ns)
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
        // The register update and the host timer (re)registration share this
        // one sleepable guard, so concurrent writes cannot reorder a stale
        // timer arm after a newer reload. The hard-timer callback retires the
        // previous arm through `TimerRegistration`'s short IRQ-safe state and
        // never takes this mutex.
        let mut service = self.service.lock();
        if let Some(plan) = service.core.handle_write(port, width, val, now_ns)? {
            service.timer.schedule(plan.deadline_ns, plan.period_ns)?;
        }
        Ok(())
    }

    /// Quiesces the IRQ0 host timer before a task-side VM pause is ACKed.
    ///
    /// The 8254 register file is retained so [`Self::resume`] re-arms the same
    /// countdown from its canonical deadline. A host cancellation failure keeps
    /// the retained handle and returns the error so the caller retries.
    pub fn suspend(&self) -> X86VlapicResult {
        let mut service = self.service.lock();
        if service.suspended {
            return Ok(());
        }
        // The full cancel barrier runs even when the callback already
        // completed, so the pause ACK only happens once no callback or payload
        // reclamation is still in flight. Only a live arm owes a resume.
        let resume_owed = service
            .timer
            .registration
            .invalidate_and_cancel(&service.timer.runtime)?;
        service.suspended = resume_owed;
        Ok(())
    }

    /// Reinstalls the IRQ0 host timer quiesced by [`Self::suspend`].
    ///
    /// Re-arming happens at most once per suspend; a second resume is a no-op,
    /// and a registration failure keeps the suspend outstanding for retry.
    pub fn resume(&self) -> X86VlapicResult {
        let mut service = self.service.lock();
        if !service.suspended {
            return Ok(());
        }
        if service.timer.registration.has_registration() {
            service.suspended = false;
            return Ok(());
        }
        let Some(period_ns) = service.core.state.channel0.period_ns else {
            service.suspended = false;
            return Ok(());
        };
        let deadline_ns = service.timer.deadline_ns.load(Ordering::Acquire);
        if deadline_ns == 0 {
            service.suspended = false;
            return Ok(());
        }
        let repeat_ns = service
            .core
            .state
            .channel0
            .mode
            .is_periodic_irq()
            .then_some(period_ns)
            .map(limit_periodic_timer_period_ns);
        service.timer.schedule(deadline_ns, repeat_ns)?;
        service.suspended = false;
        Ok(())
    }

    /// Cancels the IRQ0 host timer and retires the guest-visible PIT state.
    ///
    /// A host cancellation failure keeps both the retained handle and the
    /// register file so the caller can retry instead of dropping a live timer.
    pub fn stop(&self) -> X86VlapicResult {
        let mut service = self.service.lock();
        service.timer.cancel()?;
        service.timer.deadline_ns.store(0, Ordering::Release);
        service.core.reset();
        service.suspended = false;
        Ok(())
    }
}
