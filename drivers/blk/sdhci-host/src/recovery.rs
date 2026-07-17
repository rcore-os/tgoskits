//! Controller recovery, DMA quiescence, and reinitialization lifecycle.

use crate::*;

impl SdioHost2Lifecycle for Sdhci {
    type RecoveryState = SdhciRecoveryState;

    fn begin_recovery(
        &mut self,
        _cause: rdif_block::RecoveryCause,
    ) -> Result<Self::RecoveryState, Error> {
        if self.completion_irq_enabled() || self.recovery_quiesced {
            return Err(Error::Busy);
        }
        if self.reset_hook.as_ref().is_some_and(|hook| {
            hook.recovery_mode() == crate::host::ResetHookRecoveryMode::Unsupported
        }) {
            return Err(Error::UnsupportedCommand);
        }
        Ok(SdhciRecoveryState {
            phase: SdhciRecoveryPhase::Start,
            saved: SdhciRecoveryRegisters {
                power_control: self.read_u8(REG_POWER_CONTROL),
                clock_control: self.read_u16(REG_CLOCK_CONTROL),
                host_control1: self.read_u8(REG_HOST_CONTROL1),
                host_control2: self.read_u16(REG_HOST_CONTROL2),
                timeout_control: self.read_u8(REG_TIMEOUT_CONTROL),
                normal_status_enable: self.read_u16(REG_NORMAL_INT_STATUS_ENABLE),
                error_status_enable: self.read_u16(REG_ERROR_INT_STATUS_ENABLE),
            },
        })
    }

    fn poll_dma_quiesce(
        &mut self,
        state: &mut Self::RecoveryState,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()> {
        match state.phase {
            SdhciRecoveryPhase::Start => {
                self.write_u16(REG_NORMAL_INT_SIGNAL_ENABLE, 0);
                self.write_u16(REG_ERROR_INT_SIGNAL_ENABLE, 0);
                self.use_dma = false;
                match self.begin_before_reset_all_hook(input.now_ns) {
                    Ok(ResetHookPoll::Ready) => {
                        start_sdhci_recovery_reset(self, state, input.now_ns)
                    }
                    Ok(ResetHookPoll::Pending { wake_at_ns }) => {
                        state.phase = SdhciRecoveryPhase::WaitHook { wake_at_ns };
                        rdif_block::InitPoll::Pending(rdif_block::InitSchedule::wait_until(
                            wake_at_ns,
                        ))
                    }
                    Err(_) => fail_sdhci_recovery(state, "SDHCI platform reset preparation failed"),
                }
            }
            SdhciRecoveryPhase::WaitHook { wake_at_ns } => {
                if input.now_ns < wake_at_ns {
                    return rdif_block::InitPoll::Pending(rdif_block::InitSchedule::wait_until(
                        wake_at_ns,
                    ));
                }
                match self.poll_before_reset_all_hook(input.now_ns) {
                    Ok(ResetHookPoll::Ready) => {
                        start_sdhci_recovery_reset(self, state, input.now_ns)
                    }
                    Ok(ResetHookPoll::Pending { wake_at_ns }) => {
                        state.phase = SdhciRecoveryPhase::WaitHook { wake_at_ns };
                        rdif_block::InitPoll::Pending(rdif_block::InitSchedule::wait_until(
                            wake_at_ns,
                        ))
                    }
                    Err(_) => fail_sdhci_recovery(state, "SDHCI platform reset preparation failed"),
                }
            }
            SdhciRecoveryPhase::WaitReset { deadline_ns } => {
                if self.read_u8(REG_SOFTWARE_RESET) & RESET_ALL != 0 {
                    if input.now_ns >= deadline_ns {
                        state.phase = SdhciRecoveryPhase::Failed;
                        return rdif_block::InitPoll::Failed(rdif_block::InitError::TimedOut);
                    }
                    return rdif_block::InitPoll::Pending(recovery_wait_schedule(
                        input.now_ns,
                        deadline_ns,
                    ));
                }
                if self.call_after_reset_hook().is_err() {
                    state.phase = SdhciRecoveryPhase::Failed;
                    return rdif_block::InitPoll::Failed(rdif_block::InitError::Hardware(
                        "SDHCI platform reset reconstruction failed",
                    ));
                }
                // Runtime has drained the registered action before entering
                // this lifecycle, so recovery may explicitly take destructive
                // status ownership until normal IRQ delivery is restored.
                self.take_recovery_status_ownership();
                self.irq.state.set_delivery_enabled(false);
                self.ack_irq_status(NORMAL_INT_CLEAR_ALL, ERROR_INT_CLEAR_ALL);
                let _ = self.irq.state.take_snapshot();
                self.pending_irq = crate::host::IrqSnapshot::empty();
                self.pending_data = None;
                self.active_data_cmd = 0;
                self.command_state = command::CommandState::Idle;
                self.recovery_quiesced = true;
                state.phase = SdhciRecoveryPhase::Quiesced;
                rdif_block::InitPoll::Ready(())
            }
            SdhciRecoveryPhase::Quiesced
            | SdhciRecoveryPhase::Restore
            | SdhciRecoveryPhase::WaitClock { .. }
            | SdhciRecoveryPhase::Ready
            | SdhciRecoveryPhase::Failed => {
                rdif_block::InitPoll::Failed(rdif_block::InitError::InvalidState)
            }
        }
    }

    fn begin_reinitialize(&mut self, state: &mut Self::RecoveryState) -> Result<(), Error> {
        if !matches!(state.phase, SdhciRecoveryPhase::Quiesced) || !self.recovery_quiesced {
            return Err(Error::InvalidArgument);
        }
        state.phase = SdhciRecoveryPhase::Restore;
        Ok(())
    }

    fn poll_reinitialize(
        &mut self,
        state: &mut Self::RecoveryState,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()> {
        match state.phase {
            SdhciRecoveryPhase::Restore => {
                self.write_u8(REG_POWER_CONTROL, state.saved.power_control);
                self.write_u8(REG_HOST_CONTROL1, state.saved.host_control1);
                self.write_u16(REG_HOST_CONTROL2, state.saved.host_control2);
                self.write_u8(REG_TIMEOUT_CONTROL, state.saved.timeout_control);
                self.write_u16(
                    REG_NORMAL_INT_STATUS_ENABLE,
                    state.saved.normal_status_enable,
                );
                self.write_u16(REG_ERROR_INT_STATUS_ENABLE, state.saved.error_status_enable);
                self.write_u16(REG_NORMAL_INT_SIGNAL_ENABLE, 0);
                self.write_u16(REG_ERROR_INT_SIGNAL_ENABLE, 0);

                let requested_clock =
                    state.saved.clock_control & !(CLOCK_INTERNAL_STABLE | CLOCK_SD_ENABLE);
                self.write_u16(REG_CLOCK_CONTROL, requested_clock);
                if requested_clock & CLOCK_INTERNAL_ENABLE == 0 {
                    return finish_sdhci_reinitialization(self, state);
                }
                let deadline_ns = input.now_ns.saturating_add(RECOVERY_TRANSITION_TIMEOUT_NS);
                state.phase = SdhciRecoveryPhase::WaitClock { deadline_ns };
                rdif_block::InitPoll::Pending(recovery_wait_schedule(input.now_ns, deadline_ns))
            }
            SdhciRecoveryPhase::WaitClock { deadline_ns } => {
                let clock = self.read_u16(REG_CLOCK_CONTROL);
                if clock & CLOCK_INTERNAL_STABLE != 0 {
                    if state.saved.clock_control & CLOCK_SD_ENABLE != 0 {
                        self.write_u16(REG_CLOCK_CONTROL, clock | CLOCK_SD_ENABLE);
                    }
                    return finish_sdhci_reinitialization(self, state);
                }
                if input.now_ns >= deadline_ns {
                    state.phase = SdhciRecoveryPhase::Failed;
                    return rdif_block::InitPoll::Failed(rdif_block::InitError::TimedOut);
                }
                rdif_block::InitPoll::Pending(recovery_wait_schedule(input.now_ns, deadline_ns))
            }
            SdhciRecoveryPhase::Start
            | SdhciRecoveryPhase::WaitHook { .. }
            | SdhciRecoveryPhase::WaitReset { .. }
            | SdhciRecoveryPhase::Quiesced
            | SdhciRecoveryPhase::Ready
            | SdhciRecoveryPhase::Failed => {
                rdif_block::InitPoll::Failed(rdif_block::InitError::InvalidState)
            }
        }
    }
}

fn start_sdhci_recovery_reset(
    host: &mut Sdhci,
    state: &mut SdhciRecoveryState,
    now_ns: u64,
) -> rdif_block::InitPoll<()> {
    host.write_u8(REG_SOFTWARE_RESET, RESET_ALL);
    let deadline_ns = now_ns.saturating_add(RECOVERY_TRANSITION_TIMEOUT_NS);
    state.phase = SdhciRecoveryPhase::WaitReset { deadline_ns };
    rdif_block::InitPoll::Pending(recovery_wait_schedule(now_ns, deadline_ns))
}

fn fail_sdhci_recovery(
    state: &mut SdhciRecoveryState,
    message: &'static str,
) -> rdif_block::InitPoll<()> {
    state.phase = SdhciRecoveryPhase::Failed;
    rdif_block::InitPoll::Failed(rdif_block::InitError::Hardware(message))
}

fn finish_sdhci_reinitialization(
    host: &mut Sdhci,
    state: &mut SdhciRecoveryState,
) -> rdif_block::InitPoll<()> {
    debug_assert!(host.initialization_status_owned());
    host.ack_irq_status(NORMAL_INT_CLEAR_ALL, ERROR_INT_CLEAR_ALL);
    let _ = host.irq.state.take_snapshot();
    host.pending_irq = crate::host::IrqSnapshot::empty();
    host.dma_poisoned = false;
    host.recovery_quiesced = false;
    state.phase = SdhciRecoveryPhase::Ready;
    rdif_block::InitPoll::Ready(())
}

fn recovery_wait_schedule(now_ns: u64, deadline_ns: u64) -> rdif_block::InitSchedule {
    rdif_block::InitSchedule::wait_until(
        now_ns
            .saturating_add(RECOVERY_CHECK_INTERVAL_NS)
            .min(deadline_ns),
    )
}
