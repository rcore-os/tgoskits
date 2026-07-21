//! Controller recovery and reconstruction lifecycle.

use super::*;

const RECOVERY_CHECK_INTERVAL_NS: u64 = 50_000;
const RECOVERY_TRANSITION_TIMEOUT_NS: u64 = 100_000_000;

/// Bounded DW_mshc reset/reconstruction state retained by RDIF.
pub struct DwMmcRecoveryState {
    phase: DwMmcRecoveryPhase,
    saved: DwMmcRecoveryRegisters,
}

#[derive(Clone, Copy)]
struct DwMmcRecoveryRegisters {
    power_enable: u32,
    clock_enable: u32,
    clock_source: u32,
    clock_divider: u32,
    card_type: u32,
    uhs: u32,
    timeout: u32,
    fifo_threshold: u32,
}

enum DwMmcRecoveryPhase {
    Start,
    WaitReset { deadline_ns: u64 },
    Quiesced,
    Restore,
    WaitClockGate { deadline_ns: u64 },
    ProgramDivider,
    WaitDivider { deadline_ns: u64 },
    EnableClock,
    WaitClockEnable { deadline_ns: u64 },
    Ready,
    Failed,
}

impl SdioHost2Lifecycle for DwMmc {
    type RecoveryState = DwMmcRecoveryState;

    fn begin_recovery(
        &mut self,
        _cause: rdif_block::RecoveryCause,
    ) -> Result<Self::RecoveryState, Error> {
        if self.completion_irq_enabled() || self.recovery_quiesced {
            return Err(Error::Busy);
        }
        Ok(DwMmcRecoveryState {
            phase: DwMmcRecoveryPhase::Start,
            saved: DwMmcRecoveryRegisters {
                power_enable: self.regs.pwren().read(),
                clock_enable: self.regs.clkena().read().into_bits(),
                clock_source: self.regs.clksrc().read(),
                clock_divider: self.regs.clkdiv().read().into_bits(),
                card_type: self.regs.ctype().read().into_bits(),
                uhs: self.regs.uhs().read().into_bits(),
                timeout: self.regs.tmout().read(),
                fifo_threshold: self.regs.fifoth().read(),
            },
        })
    }

    fn poll_dma_quiesce(
        &mut self,
        state: &mut Self::RecoveryState,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()> {
        match state.phase {
            DwMmcRecoveryPhase::Start => {
                self.regs.intmask().write(0);
                self.disable_dma_for_controller_recovery();
                self.regs.clkena().write(crate::regs::ClkEna::new());
                self.regs.ctrl().update(|control| {
                    control
                        .with_controller_reset(true)
                        .with_fifo_reset(true)
                        .with_dma_reset(true)
                });
                let deadline_ns = input.now_ns.saturating_add(RECOVERY_TRANSITION_TIMEOUT_NS);
                state.phase = DwMmcRecoveryPhase::WaitReset { deadline_ns };
                rdif_block::InitPoll::Pending(dwmmc_recovery_wait(input.now_ns, deadline_ns))
            }
            DwMmcRecoveryPhase::WaitReset { deadline_ns } => {
                let control = self.regs.ctrl().read();
                if control.controller_reset() || control.fifo_reset() || control.dma_reset() {
                    if input.now_ns >= deadline_ns {
                        state.phase = DwMmcRecoveryPhase::Failed;
                        return rdif_block::InitPoll::Failed(rdif_block::InitError::TimedOut);
                    }
                    return rdif_block::InitPoll::Pending(dwmmc_recovery_wait(
                        input.now_ns,
                        deadline_ns,
                    ));
                }
                // The runtime drained the IRQ action before calling this
                // lifecycle, so destructive status ownership is temporarily
                // task-local until the action is enabled again.
                self.clear_all_int_status();
                self.clear_all_idmac_status();
                self.clear_task_irq_evidence();
                self.pending_data = None;
                self.data_blocks_remaining = 0;
                self.data_cmd_index = 0;
                self.command_state = command::CommandState::Idle;
                self.recovery_quiesced = true;
                state.phase = DwMmcRecoveryPhase::Quiesced;
                rdif_block::InitPoll::Ready(())
            }
            DwMmcRecoveryPhase::Quiesced
            | DwMmcRecoveryPhase::Restore
            | DwMmcRecoveryPhase::WaitClockGate { .. }
            | DwMmcRecoveryPhase::ProgramDivider
            | DwMmcRecoveryPhase::WaitDivider { .. }
            | DwMmcRecoveryPhase::EnableClock
            | DwMmcRecoveryPhase::WaitClockEnable { .. }
            | DwMmcRecoveryPhase::Ready
            | DwMmcRecoveryPhase::Failed => {
                rdif_block::InitPoll::Failed(rdif_block::InitError::InvalidState)
            }
        }
    }

    fn begin_reinitialize(&mut self, state: &mut Self::RecoveryState) -> Result<(), Error> {
        if !matches!(state.phase, DwMmcRecoveryPhase::Quiesced) || !self.recovery_quiesced {
            return Err(Error::InvalidArgument);
        }
        state.phase = DwMmcRecoveryPhase::Restore;
        Ok(())
    }

    fn poll_reinitialize(
        &mut self,
        state: &mut Self::RecoveryState,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()> {
        match state.phase {
            DwMmcRecoveryPhase::Restore => {
                self.regs.pwren().write(state.saved.power_enable);
                self.regs.clksrc().write(state.saved.clock_source);
                self.regs
                    .ctype()
                    .write(crate::regs::CType::from_bits(state.saved.card_type));
                self.regs
                    .uhs()
                    .write(crate::regs::UHS::from_bits(state.saved.uhs));
                self.regs.tmout().write(state.saved.timeout);
                self.regs.fifoth().write(state.saved.fifo_threshold);
                self.regs.intmask().write(0);
                self.regs.ctrl().update(|control| {
                    control
                        .with_use_internal_dmac(false)
                        .with_dma_enable(false)
                        .with_int_enable(false)
                });
                self.regs.clkena().write(crate::regs::ClkEna::new());
                issue_dwmmc_clock_update(self);
                let deadline_ns = input.now_ns.saturating_add(RECOVERY_TRANSITION_TIMEOUT_NS);
                state.phase = DwMmcRecoveryPhase::WaitClockGate { deadline_ns };
                rdif_block::InitPoll::Pending(dwmmc_recovery_wait(input.now_ns, deadline_ns))
            }
            DwMmcRecoveryPhase::WaitClockGate { deadline_ns } => {
                if self.regs.cmd().read().start_cmd() {
                    return pending_or_timeout(state, input.now_ns, deadline_ns);
                }
                state.phase = DwMmcRecoveryPhase::ProgramDivider;
                rdif_block::InitPoll::Pending(rdif_block::InitSchedule::immediate())
            }
            DwMmcRecoveryPhase::ProgramDivider => {
                self.regs
                    .clkdiv()
                    .write(crate::regs::ClkDiv::from_bits(state.saved.clock_divider));
                issue_dwmmc_clock_update(self);
                let deadline_ns = input.now_ns.saturating_add(RECOVERY_TRANSITION_TIMEOUT_NS);
                state.phase = DwMmcRecoveryPhase::WaitDivider { deadline_ns };
                rdif_block::InitPoll::Pending(dwmmc_recovery_wait(input.now_ns, deadline_ns))
            }
            DwMmcRecoveryPhase::WaitDivider { deadline_ns } => {
                if self.regs.cmd().read().start_cmd() {
                    return pending_or_timeout(state, input.now_ns, deadline_ns);
                }
                state.phase = DwMmcRecoveryPhase::EnableClock;
                rdif_block::InitPoll::Pending(rdif_block::InitSchedule::immediate())
            }
            DwMmcRecoveryPhase::EnableClock => {
                self.regs
                    .clkena()
                    .write(crate::regs::ClkEna::from_bits(state.saved.clock_enable));
                issue_dwmmc_clock_update(self);
                let deadline_ns = input.now_ns.saturating_add(RECOVERY_TRANSITION_TIMEOUT_NS);
                state.phase = DwMmcRecoveryPhase::WaitClockEnable { deadline_ns };
                rdif_block::InitPoll::Pending(dwmmc_recovery_wait(input.now_ns, deadline_ns))
            }
            DwMmcRecoveryPhase::WaitClockEnable { deadline_ns } => {
                if self.regs.cmd().read().start_cmd() {
                    return pending_or_timeout(state, input.now_ns, deadline_ns);
                }
                self.clear_all_int_status();
                self.clear_all_idmac_status();
                self.clear_task_irq_evidence();
                self.dma_poisoned = false;
                self.recovery_quiesced = false;
                state.phase = DwMmcRecoveryPhase::Ready;
                rdif_block::InitPoll::Ready(())
            }
            DwMmcRecoveryPhase::Start
            | DwMmcRecoveryPhase::WaitReset { .. }
            | DwMmcRecoveryPhase::Quiesced
            | DwMmcRecoveryPhase::Ready
            | DwMmcRecoveryPhase::Failed => {
                rdif_block::InitPoll::Failed(rdif_block::InitError::InvalidState)
            }
        }
    }
}

fn issue_dwmmc_clock_update(host: &DwMmc) {
    host.regs.cmd().write(
        crate::regs::Cmd::new()
            .with_use_hold_reg(false)
            .with_wait_prvdata_complete(false)
            .with_update_clock_registers_only(true),
    );
}

fn dwmmc_recovery_wait(now_ns: u64, deadline_ns: u64) -> rdif_block::InitSchedule {
    rdif_block::InitSchedule::wait_until(
        now_ns
            .saturating_add(RECOVERY_CHECK_INTERVAL_NS)
            .min(deadline_ns),
    )
}

fn pending_or_timeout(
    state: &mut DwMmcRecoveryState,
    now_ns: u64,
    deadline_ns: u64,
) -> rdif_block::InitPoll<()> {
    if now_ns >= deadline_ns {
        state.phase = DwMmcRecoveryPhase::Failed;
        rdif_block::InitPoll::Failed(rdif_block::InitError::TimedOut)
    } else {
        rdif_block::InitPoll::Pending(dwmmc_recovery_wait(now_ns, deadline_ns))
    }
}
