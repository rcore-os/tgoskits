use super::*;

pub(super) fn next_init_activation<'a, H: SdioHost + 'a>(
    request: &mut SdioInitRequest<'a, H>,
    now_ns: u64,
) -> PendingActivation {
    if matches!(
        request.state,
        SdioInitState::PostPowerOnDelay
            | SdioInitState::PostIdentificationClockDelay
            | SdioInitState::WaitAcmd41Retry
            | SdioInitState::WaitMmcRetry
    ) {
        request.deadline_state = None;
        request.hardware_deadline_ns = None;
        let retry_at = request.retry_at_ns.unwrap_or(now_ns);
        let wake_at = request
            .power_deadline_ns
            .map_or(retry_at, |deadline| retry_at.min(deadline));
        return PendingActivation::wait_until(wake_at);
    }

    if let Some(switch_request) = request.mmc_switch_request.as_ref() {
        request.deadline_state = None;
        request.hardware_deadline_ns = None;
        return match switch_request.state {
            MmcSwitchRequestState::PollSwitch | MmcSwitchRequestState::PollStatus => {
                wait_for_controller_progress(
                    request.transaction_wake_at_ns,
                    switch_request.deadline_ns,
                )
            }
            MmcSwitchRequestState::WaitStatusRetry => {
                let retry_at = switch_request.retry_at_ns.unwrap_or(now_ns);
                PendingActivation::wait_until(retry_at.min(switch_request.deadline_ns))
            }
        };
    }

    if request.bus_request.is_some() {
        let deadline = request_deadline(request, now_ns, INIT_EVENTLESS_TIMEOUT_NS);
        if let Some(wake_at_ns) = request.bus_wake_at_ns {
            return if wake_at_ns >= deadline {
                PendingActivation::timeout_at(deadline)
            } else {
                PendingActivation::wait_until(wake_at_ns)
            };
        }
        let next_check = now_ns.saturating_add(INIT_EVENTLESS_POLL_NS);
        return if next_check >= deadline {
            PendingActivation::timeout_at(deadline)
        } else {
            PendingActivation::wait_until(next_check)
        };
    }

    if state_waits_for_controller_irq(request.state) {
        let first_activation = request.deadline_state != Some(request.state);
        let deadline = request_deadline(request, now_ns, INIT_IRQ_TIMEOUT_NS);
        if first_activation {
            // A timed host may need one task-context transition to publish an
            // eventless register-programming deadline before hardware can
            // raise the first completion IRQ. This is a programming step,
            // never a completion probe.
            return PendingActivation::immediate();
        }
        return wait_for_controller_progress(request.transaction_wake_at_ns, deadline);
    }

    request.deadline_state = None;
    request.hardware_deadline_ns = None;
    request.transaction_wake_at_ns = None;
    PendingActivation::immediate()
}

fn wait_for_controller_progress(
    transaction_wake_at_ns: Option<u64>,
    watchdog_deadline_ns: u64,
) -> PendingActivation {
    match transaction_wake_at_ns {
        Some(wake_at_ns) if wake_at_ns < watchdog_deadline_ns => {
            PendingActivation::wait_for_irq_or_until(wake_at_ns)
        }
        _ => PendingActivation::wait_for_irq(watchdog_deadline_ns),
    }
}

fn request_deadline<'a, H: SdioHost + 'a>(
    request: &mut SdioInitRequest<'a, H>,
    now_ns: u64,
    timeout_ns: u64,
) -> u64 {
    if request.deadline_state != Some(request.state) {
        request.deadline_state = Some(request.state);
        request.hardware_deadline_ns = Some(now_ns.saturating_add(timeout_ns));
    }
    request
        .hardware_deadline_ns
        .unwrap_or_else(|| now_ns.saturating_add(timeout_ns))
}

fn state_waits_for_controller_irq(state: SdioInitState) -> bool {
    matches!(
        state,
        SdioInitState::PollCmd0
            | SdioInitState::PollCmd8
            | SdioInitState::PollAcmd41Cmd55
            | SdioInitState::PollAcmd41
            | SdioInitState::PollMmcInitial
            | SdioInitState::PollMmcReady
            | SdioInitState::PollCmd2
            | SdioInitState::PollCmd3
            | SdioInitState::PollCmd9
            | SdioInitState::PollCmd7
            | SdioInitState::PollSdBusWidthCmd55
            | SdioInitState::PollSdBusWidthAcmd6
            | SdioInitState::PollMmcExtCsd
            | SdioInitState::PollMmcHs200Status
            | SdioInitState::PollSdSwitchFunctionCheck
            | SdioInitState::PollSdVoltageSwitch
            | SdioInitState::PollSdSetAccessMode
            | SdioInitState::PollSdStatus
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_programming_deadline_keeps_controller_irq_armed() {
        let activation = wait_for_controller_progress(Some(11_000), 1_000_001_000);

        assert_eq!(
            activation.schedule,
            InitSchedule {
                run_again: false,
                irq: crate::sdio::init_schedule::InitIrqWait::Controller,
                wake_at_ns: Some(11_000),
            }
        );
        for now_ns in [1_000, 1_001, 10_999] {
            assert_eq!(
                activation.activation(InitInput::at(now_ns)),
                Activation::Waiting
            );
        }
        assert_eq!(
            activation.activation(InitInput::at(11_000)),
            Activation::Advance
        );
        assert_eq!(
            activation.activation(InitInput::with_controller_irq(5_000)),
            Activation::Advance
        );
    }

    #[test]
    fn watchdog_wins_when_programming_deadline_is_not_earlier() {
        let activation = wait_for_controller_progress(Some(2_000), 2_000);

        assert_eq!(
            activation.activation(InitInput::at(1_999)),
            Activation::Waiting
        );
        assert_eq!(
            activation.activation(InitInput::at(2_000)),
            Activation::Timeout
        );
    }
}
