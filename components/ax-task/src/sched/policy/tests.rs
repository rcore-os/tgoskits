use super::*;

#[test]
fn uses_linux_nice_weights() {
    assert_eq!(Nice::new(-20).unwrap().weight(), 88_761);
    assert_eq!(Nice::ZERO.weight(), 1_024);
    assert_eq!(Nice::new(19).unwrap().weight(), 15);
}

#[test]
fn kernel_stopper_outranks_deadline_rt_and_fair_work() {
    let stopper = SchedulePolicy::kernel_stop();
    let deadline =
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap());
    let realtime = SchedulePolicy::fifo(RtPriority::new(99).unwrap());
    let fair = SchedulePolicy::default();

    assert!(stopper.scheduling_key(0) < deadline.scheduling_key(0));
    assert!(deadline.scheduling_key(0) < realtime.scheduling_key(0));
    assert!(realtime.scheduling_key(0) < fair.scheduling_key(0));
}

#[test]
fn deadline_parameters_reserve_the_msb_for_linux_wrap_ordering() {
    let half_range = 1_u64 << 63;

    assert!(DeadlinePolicy::new(1, 1, half_range - 1, DeadlineFlags::NONE).is_ok());
    assert!(DeadlinePolicy::new(1, 1, half_range, DeadlineFlags::NONE).is_err());
    assert!(DeadlinePolicy::new(1, half_range, half_range, DeadlineFlags::NONE).is_err());
}
