mod clone;
mod clone3;
mod ctl;
mod execve;
mod exit;
mod job;
mod namespace;
pub mod ptrace;
mod schedule;
mod thread;
mod wait;

pub use self::{
    clone::*, clone3::*, ctl::*, execve::*, exit::*, job::*, namespace::*, ptrace::*, schedule::*,
    thread::*, wait::*,
};

#[cfg(axtest)]
pub(crate) fn clone_validation_rules_hold_for_test() -> bool {
    clone::clone_validation_rules_hold_for_test() && clone3::clone3_validation_rules_hold_for_test()
}

#[cfg(axtest)]
pub(crate) fn capability_data_conversion_rules_hold_for_test() -> bool {
    ctl::capability_data_conversion_rules_hold_for_test()
}

#[cfg(axtest)]
pub(crate) use self::exit::exit_code_encoding_rules_hold_for_test;
#[cfg(axtest)]
pub(crate) use self::job::job_setpgid_validation_rules_hold_for_test;
#[cfg(axtest)]
pub(crate) use self::schedule::schedule_clock_and_sched_validation_rules_hold_for_test;
#[cfg(axtest)]
pub(crate) use self::thread::thread_arch_prctl_code_rules_hold_for_test;
