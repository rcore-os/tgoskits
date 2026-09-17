#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_remains_unpublished_until_group_accounting_requests_it() {
        let accounting = CpuTimeAccounting::new().unwrap();


        assert_eq!(
            accounting.unpublished_delta(10),
            CpuTimeDelta {
                raw_user_ns: 0,
                raw_system_ns: 0,
                runtime_ns: 10,
            },
            "runtime stays thread-local until process accounting requests publication"
        );

        assert_eq!(
            accounting.publish_committed_delta(10),
            CpuTimeDelta {
                raw_user_ns: 0,
                raw_system_ns: 0,
                runtime_ns: 10,
            },
            "active group accounting must publish the task-local runtime"
        );
        assert_eq!(
            accounting.unpublished_delta(10),
            CpuTimeDelta::ZERO,
            "published runtime must not be added again by a group reader"
        );
    }

    #[test]
    fn adjusted_cpu_time_matches_linux_monotonic_runtime_contract() {
        let high_water = RawSpinLock::new(CpuTimeHighWater::ZERO);

        assert_eq!(
            adjust_cpu_time(0, 0, 10, &high_water),
            CpuTimeHighWater {
                user_ns: 10,
                system_ns: 0,
            }
        );
        assert_eq!(
            adjust_cpu_time(10, 10, 20, &high_water),
            CpuTimeHighWater {
                user_ns: 10,
                system_ns: 10,
            }
        );
        assert_eq!(
            adjust_cpu_time(1, 9, 15, &high_water),
            CpuTimeHighWater {
                user_ns: 10,
                system_ns: 10,
            },
            "a stale runtime sample must return the prior high-water mark"
        );
    }
}
