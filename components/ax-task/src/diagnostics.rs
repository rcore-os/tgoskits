//! Feature-gated scheduler observations, independent of scheduling policy.

#[cfg(feature = "qperf-metrics")]
pub use crate::diagnostics::counters::QperfSchedulerMetricsSnapshot;
#[cfg(feature = "qperf-metrics")]
pub use crate::diagnostics::counters::qperf_record_switch_phase_owner_tail;
#[cfg(feature = "qperf-metrics")]
pub use crate::diagnostics::counters::qperf_record_switch_phase_prepare;
#[cfg(feature = "qperf-metrics")]
pub use crate::diagnostics::counters::qperf_record_switch_phase_runtime_tail;
#[cfg(feature = "qperf-metrics")]
pub use crate::diagnostics::counters::qperf_record_switch_phase_scheduler;
#[cfg(feature = "qperf-metrics")]
pub use crate::diagnostics::counters::qperf_record_switch_scheduler_detail;
#[cfg(feature = "qperf-metrics")]
pub use crate::diagnostics::counters::qperf_scheduler_metrics_snapshot;
#[cfg(axtest)]
pub use crate::sched::system::PiScheduleTestProbeSnapshot;
#[cfg(axtest)]
pub use crate::sched::system::begin_pi_schedule_test_probe;
#[cfg(axtest)]
pub use crate::sched::system::end_pi_schedule_test_probe;

#[cfg(feature = "qperf-metrics")]
pub(crate) mod counters;

#[cfg(axtest)]
pub use crate::sched::system::pi_schedule_test_probe_snapshot;
