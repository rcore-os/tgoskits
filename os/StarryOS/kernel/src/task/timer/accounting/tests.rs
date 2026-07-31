#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preemption_and_yield_preserve_rttime_but_block_resets_it() {
        let accounting = CpuTimeAccounting::new();
        accounting.set_state_at(TimerState::User, 0);
        accounting.scheduler_switch_in_at(true, 0);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Preempted, 500_000);
        assert_eq!(
            accounting.snapshot_at(500_000).realtime_continuous_ns,
            500_000
        );

        accounting.scheduler_switch_in_at(true, 500_000);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Yield, 1_000_000);
        assert_eq!(
            accounting.snapshot_at(1_000_000).realtime_continuous_ns,
            1_000_000
        );

        accounting.scheduler_switch_in_at(true, 1_000_000);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Preempted, 1_500_000);
        assert_eq!(
            accounting.snapshot_at(1_500_000).realtime_continuous_ns,
            1_500_000
        );

        accounting.scheduler_switch_in_at(true, 1_500_000);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Blocked, 2_000_000);
        let blocked = accounting.snapshot_at(2_000_000);
        assert_eq!(blocked.realtime_continuous_ns, 0);
        assert_eq!(blocked.realtime_reset_generation, 1);
    }

    #[test]
    fn leaving_rt_policy_resets_continuous_runtime() {
        let accounting = CpuTimeAccounting::new();
        accounting.set_state_at(TimerState::Kernel, 0);
        accounting.scheduler_switch_in_at(true, 0);
        accounting.set_realtime_policy_at(false, 2_000_000);
        let fair = accounting.snapshot_at(3_000_000);
        assert_eq!(fair.realtime_continuous_ns, 0);
        assert_eq!(fair.system_ns, 3_000_000);

        accounting.set_realtime_policy_at(true, 3_000_000);
        assert_eq!(
            accounting.snapshot_at(3_500_000).realtime_continuous_ns,
            500_000
        );
    }

    #[test]
    fn owner_policy_update_closes_its_sequence_epoch() {
        let accounting = CpuTimeAccounting::new();
        accounting.scheduler_switch_in_at(true, 0);

        accounting.set_realtime_policy_at(false, 1_000_000);

        assert_eq!(accounting.sequence.load(Ordering::Acquire), 4);
        assert_eq!(accounting.snapshot_at(2_000_000).realtime_continuous_ns, 0);
    }

    #[test]
    fn scheduler_tick_sampling_is_read_only() {
        let accounting = CpuTimeAccounting::new();
        accounting.set_state_at(TimerState::User, 0);
        accounting.scheduler_switch_in_at(false, 0);

        let sequence = accounting.sequence.load(Ordering::Acquire);
        assert_eq!(
            accounting.sample_scheduler_tick_at(10),
            CpuTimeDelta {
                user_ns: 10,
                system_ns: 0,
            }
        );
        assert_eq!(accounting.sequence.load(Ordering::Acquire), sequence);
        assert_eq!(accounting.user_ns.load(Ordering::Acquire), 0);
    }
}
