//! Controller-wide Phytium MCI quiesce and reconstruction state machine.

use rdif_block::{InitError, InitInput, InitPoll, InitSchedule, RecoveryCause};
use sdmmc_protocol::{error::Error, sdio::host2::SdioHost2Lifecycle};

use crate::{
    PhytiumMci,
    command::CommandState,
    regs::{CType, ClkEna, Cmd, RegisterBlockVolatileFieldAccess, Uhs},
};

const TRANSITION_TIMEOUT_NS: u64 = 100_000_000;
const CHECK_INTERVAL_NS: u64 = 100_000;

/// Preallocated state for one controller-wide recovery transaction.
pub struct PhytiumMciRecoveryState {
    phase: RecoveryPhase,
    saved: RecoveryRegisters,
}

#[derive(Clone, Copy)]
struct RecoveryRegisters {
    power_enable: u32,
    clock_enable: u32,
    clock_source: u32,
    clock_divider: u32,
    card_type: u32,
    uhs: u32,
    timeout: u32,
    fifo_threshold: u32,
    use_hold_reg: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecoveryPhase {
    Start,
    WaitReset { deadline_ns: u64 },
    Quiesced,
    Restore,
    WaitExternalClock { deadline_ns: u64 },
    WaitClockGate { deadline_ns: u64 },
    ProgramClock,
    WaitClockEnable { deadline_ns: u64 },
    Ready,
    Failed,
}

impl SdioHost2Lifecycle for PhytiumMci {
    type RecoveryState = PhytiumMciRecoveryState;

    fn begin_recovery(&mut self, _cause: RecoveryCause) -> Result<Self::RecoveryState, Error> {
        if self.completion_irq_enabled() || self.recovery_quiesced {
            return Err(Error::Busy);
        }
        Ok(PhytiumMciRecoveryState {
            phase: RecoveryPhase::Start,
            saved: RecoveryRegisters {
                power_enable: self.regs.pwren().read(),
                clock_enable: self.regs.clkena().read().into_bits(),
                clock_source: self.read_clock_source_raw(),
                clock_divider: self.regs.clkdiv().read(),
                card_type: self.regs.ctype().read().into_bits(),
                uhs: self.regs.uhs().read().into_bits(),
                timeout: self.regs.tmout().read(),
                fifo_threshold: self.regs.fifoth().read(),
                use_hold_reg: self.use_hold_reg,
            },
        })
    }

    fn poll_dma_quiesce(
        &mut self,
        state: &mut Self::RecoveryState,
        input: InitInput,
    ) -> InitPoll<()> {
        match state.phase {
            RecoveryPhase::Start => {
                self.regs.intmask().write(0);
                self.regs.idinten().write(0);
                self.disable_idmac();
                self.regs.clkena().write(ClkEna::new());
                self.regs.ctrl().update(|control| {
                    control
                        .with_controller_reset(true)
                        .with_fifo_reset(true)
                        .with_dma_reset(true)
                });
                let deadline_ns = input.now_ns.saturating_add(TRANSITION_TIMEOUT_NS);
                state.phase = RecoveryPhase::WaitReset { deadline_ns };
                InitPoll::Pending(wait_schedule(input.now_ns, deadline_ns))
            }
            RecoveryPhase::WaitReset { deadline_ns } => {
                let control = self.regs.ctrl().read();
                if control.controller_reset() || control.fifo_reset() || control.dma_reset() {
                    return pending_or_timeout(state, input.now_ns, deadline_ns);
                }

                // Runtime has already masked and synchronized the IRQ action,
                // so recovery temporarily owns destructive status cleanup.
                self.clear_all_int_status();
                self.regs.idsts().write(u32::MAX);
                self.irq.state.clear_all();
                self.pending_data = None;
                self.data_blocks_remaining = 0;
                self.data_cmd_index = 0;
                self.command_state = CommandState::Idle;
                self.recovery_quiesced = true;
                state.phase = RecoveryPhase::Quiesced;
                InitPoll::Ready(())
            }
            RecoveryPhase::Quiesced
            | RecoveryPhase::Restore
            | RecoveryPhase::WaitExternalClock { .. }
            | RecoveryPhase::WaitClockGate { .. }
            | RecoveryPhase::ProgramClock
            | RecoveryPhase::WaitClockEnable { .. }
            | RecoveryPhase::Ready
            | RecoveryPhase::Failed => InitPoll::Failed(InitError::InvalidState),
        }
    }

    fn begin_reinitialize(&mut self, state: &mut Self::RecoveryState) -> Result<(), Error> {
        if !matches!(state.phase, RecoveryPhase::Quiesced) || !self.recovery_quiesced {
            return Err(Error::InvalidArgument);
        }
        state.phase = RecoveryPhase::Restore;
        Ok(())
    }

    fn poll_reinitialize(
        &mut self,
        state: &mut Self::RecoveryState,
        input: InitInput,
    ) -> InitPoll<()> {
        match state.phase {
            RecoveryPhase::Restore => {
                restore_static_registers(self, state.saved);
                let deadline_ns = input.now_ns.saturating_add(TRANSITION_TIMEOUT_NS);
                state.phase = RecoveryPhase::WaitExternalClock { deadline_ns };
                InitPoll::Pending(wait_schedule(input.now_ns, deadline_ns))
            }
            RecoveryPhase::WaitExternalClock { deadline_ns } => {
                if !self.regs.cksts().read().ready() {
                    return pending_or_timeout(state, input.now_ns, deadline_ns);
                }
                self.regs.clkena().write(ClkEna::new());
                issue_clock_update(self);
                let deadline_ns = input.now_ns.saturating_add(TRANSITION_TIMEOUT_NS);
                state.phase = RecoveryPhase::WaitClockGate { deadline_ns };
                InitPoll::Pending(wait_schedule(input.now_ns, deadline_ns))
            }
            RecoveryPhase::WaitClockGate { deadline_ns } => {
                if self.regs.cmd().read().start_cmd() {
                    return pending_or_timeout(state, input.now_ns, deadline_ns);
                }
                state.phase = RecoveryPhase::ProgramClock;
                InitPoll::Pending(InitSchedule::immediate())
            }
            RecoveryPhase::ProgramClock => {
                self.regs.clkdiv().write(state.saved.clock_divider);
                self.regs
                    .clkena()
                    .write(ClkEna::from_bits(state.saved.clock_enable));
                issue_clock_update(self);
                let deadline_ns = input.now_ns.saturating_add(TRANSITION_TIMEOUT_NS);
                state.phase = RecoveryPhase::WaitClockEnable { deadline_ns };
                InitPoll::Pending(wait_schedule(input.now_ns, deadline_ns))
            }
            RecoveryPhase::WaitClockEnable { deadline_ns } => {
                if self.regs.cmd().read().start_cmd() {
                    return pending_or_timeout(state, input.now_ns, deadline_ns);
                }
                self.clear_all_int_status();
                self.regs.idsts().write(u32::MAX);
                self.irq.state.clear_all();
                self.dma_poisoned = false;
                self.recovery_quiesced = false;
                self.clear_completion_irq_enabled();
                state.phase = RecoveryPhase::Ready;
                InitPoll::Ready(())
            }
            RecoveryPhase::Start
            | RecoveryPhase::WaitReset { .. }
            | RecoveryPhase::Quiesced
            | RecoveryPhase::Ready
            | RecoveryPhase::Failed => InitPoll::Failed(InitError::InvalidState),
        }
    }
}

fn restore_static_registers(host: &mut PhytiumMci, saved: RecoveryRegisters) {
    host.regs.pwren().write(saved.power_enable);
    host.write_ext_reg(crate::regs::CLK_SRC_OFFSET, saved.clock_source);
    host.regs.ctype().write(CType::from_bits(saved.card_type));
    host.regs.uhs().write(Uhs::from_bits(saved.uhs));
    host.regs.tmout().write(saved.timeout);
    host.regs.fifoth().write(saved.fifo_threshold);
    host.regs.intmask().write(0);
    host.regs.idinten().write(0);
    host.regs.ctrl().update(|control| {
        control
            .with_use_internal_dmac(false)
            .with_dma_enable(false)
            .with_int_enable(false)
    });
    host.use_hold_reg = saved.use_hold_reg;
}

fn issue_clock_update(host: &PhytiumMci) {
    host.regs.cmd().write(
        Cmd::new()
            .with_start_cmd(true)
            .with_use_hold_reg(host.use_hold_reg)
            .with_wait_prvdata_complete(false)
            .with_update_clock_registers_only(true),
    );
}

fn wait_schedule(now_ns: u64, deadline_ns: u64) -> InitSchedule {
    InitSchedule::wait_until(now_ns.saturating_add(CHECK_INTERVAL_NS).min(deadline_ns))
}

fn pending_or_timeout(
    state: &mut PhytiumMciRecoveryState,
    now_ns: u64,
    deadline_ns: u64,
) -> InitPoll<()> {
    if now_ns >= deadline_ns {
        state.phase = RecoveryPhase::Failed;
        InitPoll::Failed(InitError::TimedOut)
    } else {
        InitPoll::Pending(wait_schedule(now_ns, deadline_ns))
    }
}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;

    use rdif_block::{InitInput, InitPoll, RecoveryCause};
    use sdmmc_protocol::sdio::host2::SdioHost2Lifecycle;

    use super::*;
    use crate::regs::{ClockStatus, Ctrl, RegisterBlockVolatileFieldAccess};

    const CTRL_WORD: usize = 0;
    const CMD_WORD: usize = 11;
    const CLOCK_STATUS_WORD: usize = 22;

    #[test]
    fn recovery_rebuilds_controller_with_bounded_reset_and_clock_transitions() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        let mut recovery = <PhytiumMci as SdioHost2Lifecycle>::begin_recovery(
            &mut host,
            RecoveryCause::QueueFault { queue_id: 0 },
        )
        .unwrap();

        let reset_wake = pending_wake(<PhytiumMci as SdioHost2Lifecycle>::poll_dma_quiesce(
            &mut host,
            &mut recovery,
            InitInput::at(1_000),
        ));
        assert!(host.regs.ctrl().read().controller_reset());
        unsafe {
            mmio.as_mut_ptr()
                .add(CTRL_WORD)
                .write_volatile(Ctrl::new().into_bits());
        }
        assert!(matches!(
            <PhytiumMci as SdioHost2Lifecycle>::poll_dma_quiesce(
                &mut host,
                &mut recovery,
                InitInput::at(reset_wake),
            ),
            InitPoll::Ready(())
        ));
        assert!(host.recovery_quiesced);

        <PhytiumMci as SdioHost2Lifecycle>::begin_reinitialize(&mut host, &mut recovery).unwrap();
        let external_clock_wake =
            pending_wake(<PhytiumMci as SdioHost2Lifecycle>::poll_reinitialize(
                &mut host,
                &mut recovery,
                InitInput::at(reset_wake + 1),
            ));
        unsafe {
            mmio.as_mut_ptr()
                .add(CLOCK_STATUS_WORD)
                .write_volatile(ClockStatus::new().with_ready(true).into_bits());
        }

        let clock_gate_wake = pending_wake(<PhytiumMci as SdioHost2Lifecycle>::poll_reinitialize(
            &mut host,
            &mut recovery,
            InitInput::at(external_clock_wake),
        ));
        assert!(host.regs.cmd().read().start_cmd());
        unsafe {
            mmio.as_mut_ptr()
                .add(CMD_WORD)
                .write_volatile(Cmd::new().with_start_cmd(false).into_bits());
        }

        assert!(matches!(
            <PhytiumMci as SdioHost2Lifecycle>::poll_reinitialize(
                &mut host,
                &mut recovery,
                InitInput::at(clock_gate_wake),
            ),
            InitPoll::Pending(schedule) if schedule.run_again()
        ));
        let clock_enable_wake =
            pending_wake(<PhytiumMci as SdioHost2Lifecycle>::poll_reinitialize(
                &mut host,
                &mut recovery,
                InitInput::at(clock_gate_wake),
            ));
        assert!(host.regs.cmd().read().start_cmd());
        unsafe {
            mmio.as_mut_ptr()
                .add(CMD_WORD)
                .write_volatile(Cmd::new().with_start_cmd(false).into_bits());
        }

        assert!(matches!(
            <PhytiumMci as SdioHost2Lifecycle>::poll_reinitialize(
                &mut host,
                &mut recovery,
                InitInput::at(clock_enable_wake),
            ),
            InitPoll::Ready(())
        ));
        assert!(!host.recovery_quiesced);
    }

    fn pending_wake(progress: InitPoll<()>) -> u64 {
        match progress {
            InitPoll::Pending(schedule) => schedule
                .wake_at_ns()
                .expect("hardware transition must have an absolute deadline"),
            _ => panic!("hardware transition must remain pending until its next activation"),
        }
    }
}
