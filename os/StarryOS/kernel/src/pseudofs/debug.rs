use alloc::sync::Arc;

#[cfg(any(feature = "qperf-metrics", feature = "uaccess-lock-regression"))]
use super::SimpleFile;
use super::{DirMaker, DirMapping, SimpleDir, SimpleFs};

const DEBUGFS_MAGIC: u32 = 0x64626720;

/// Create a new debugfs filesystem.
pub fn new_debugfs() -> axfs_ng_vfs::Filesystem {
    // TODO: update fs_type
    SimpleFs::new_with("debug".into(), DEBUGFS_MAGIC, debugfs_builder)
}

fn debugfs_builder(fs: Arc<SimpleFs>) -> DirMaker {
    let mut root = DirMapping::new();
    let tracing = crate::tracepoint::init_tracing_dir(fs.clone());
    root.add("tracing", tracing);
    #[cfg(feature = "uaccess-lock-regression")]
    root.add(
        "uaccess_lock_regression",
        SimpleFile::new_regular(
            fs.clone(),
            super::RwFile::new(
                |operation| -> axfs_ng_vfs::VfsResult<Option<alloc::vec::Vec<u8>>> {
                    match operation {
                        super::SimpleFileOperation::Read => Ok(Some(alloc::vec![
                            0;
                            crate::mm::observe_user_copy_test_state()
                        ])),
                        super::SimpleFileOperation::Write(b"hold") => {
                            if crate::mm::hold_address_space_until_user_copy() {
                                Ok(None)
                            } else {
                                Err(axfs_ng_vfs::VfsError::InvalidInput)
                            }
                        }
                        super::SimpleFileOperation::Write(_) => {
                            Err(axfs_ng_vfs::VfsError::InvalidInput)
                        }
                    }
                },
            ),
        ),
    );
    #[cfg(feature = "qperf-metrics")]
    root.add(
        "scheduler_metrics",
        SimpleFile::new_regular(fs.clone(), || Ok(render_scheduler_metrics())),
    );
    SimpleDir::new_maker(fs, Arc::new(root))
}

#[cfg(feature = "qperf-metrics")]
fn render_scheduler_metrics() -> alloc::string::String {
    use core::fmt::Write;

    let metrics = ax_runtime::task::qperf_runtime_scheduler_metrics_snapshot();
    let task = metrics.task;
    let pipe = crate::file::pipe_qperf_metrics_snapshot();
    let mut output = alloc::string::String::new();
    writeln!(output, "pipe_read_calls {}", pipe.read_calls).unwrap();
    writeln!(output, "pipe_read_waits {}", pipe.read_waits).unwrap();
    writeln!(output, "pipe_read_bytes {}", pipe.read_bytes).unwrap();
    writeln!(output, "pipe_write_calls {}", pipe.write_calls).unwrap();
    writeln!(output, "pipe_write_waits {}", pipe.write_waits).unwrap();
    writeln!(output, "pipe_write_bytes {}", pipe.write_bytes).unwrap();
    writeln!(
        output,
        "pipe_wait_registrations {}",
        pipe.wait_registrations
    )
    .unwrap();
    writeln!(
        output,
        "pipe_wait_registration_races {}",
        pipe.wait_registration_races
    )
    .unwrap();
    writeln!(output, "pipe_wake_calls {}", pipe.wake_calls).unwrap();
    writeln!(
        output,
        "pipe_wake_shared_matches {}",
        pipe.wake_shared_matches
    )
    .unwrap();
    writeln!(
        output,
        "pipe_wake_no_exclusive_match {}",
        pipe.wake_no_exclusive_match
    )
    .unwrap();
    writeln!(
        output,
        "pipe_wake_direct_attempts {}",
        pipe.wake_direct_attempts
    )
    .unwrap();
    writeln!(
        output,
        "pipe_wake_direct_delivered {}",
        pipe.wake_direct_delivered
    )
    .unwrap();
    writeln!(output, "pipe_wake_direct_retry {}", pipe.wake_direct_retry).unwrap();
    writeln!(output, "pipe_wake_direct_stale {}", pipe.wake_direct_stale).unwrap();
    writeln!(
        output,
        "pipe_wake_poll_delivered {}",
        pipe.wake_poll_delivered
    )
    .unwrap();
    writeln!(
        output,
        "current_thread_handle_queries {}",
        task.current_thread_handle_queries
    )
    .unwrap();
    writeln!(
        output,
        "scheduler_deadline_derivation_entries {}",
        task.scheduler_deadline_derivation_entries
    )
    .unwrap();
    writeln!(
        output,
        "scheduler_deadline_derivation_clock_event_entries {}",
        task.scheduler_deadline_derivation_clock_event_entries
    )
    .unwrap();
    writeln!(
        output,
        "scheduler_deadline_derivation_park_arm_entries {}",
        task.scheduler_deadline_derivation_park_arm_entries
    )
    .unwrap();
    writeln!(
        output,
        "scheduler_deadline_derivation_park_cancel_entries {}",
        task.scheduler_deadline_derivation_park_cancel_entries
    )
    .unwrap();
    writeln!(
        output,
        "scheduler_deadline_derivation_kernel_timer_entries {}",
        task.scheduler_deadline_derivation_kernel_timer_entries
    )
    .unwrap();
    writeln!(
        output,
        "scheduler_deadline_derivation_ktimer_service_entries {}",
        task.scheduler_deadline_derivation_ktimer_service_entries
    )
    .unwrap();
    writeln!(
        output,
        "scheduler_deadline_derivation_enqueue_entries {}",
        task.scheduler_deadline_derivation_enqueue_entries
    )
    .unwrap();
    writeln!(
        output,
        "scheduler_deadline_derivation_placement_entries {}",
        task.scheduler_deadline_derivation_placement_entries
    )
    .unwrap();
    writeln!(
        output,
        "scheduler_deadline_derivation_schedule_selection_entries {}",
        task.scheduler_deadline_derivation_schedule_selection_entries
    )
    .unwrap();
    writeln!(
        output,
        "scheduler_deadline_derivation_schedule_no_switch_entries {}",
        task.scheduler_deadline_derivation_schedule_no_switch_entries
    )
    .unwrap();
    writeln!(
        output,
        "runtime_preempt_guard_entries {}",
        task.runtime_preempt_guard_entries
    )
    .unwrap();
    writeln!(
        output,
        "runtime_preempt_guard_none {}",
        task.runtime_preempt_guard_none
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_ticket_entries {}",
        task.preempt_guard_ticket_entries
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_ticket_none {}",
        task.preempt_guard_ticket_none
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_explicit_entries {}",
        task.preempt_guard_explicit_entries
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_explicit_none {}",
        task.preempt_guard_explicit_none
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_sync_entries {}",
        task.preempt_guard_sync_entries
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_sync_none {}",
        task.preempt_guard_sync_none
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_activity_entries {}",
        task.preempt_guard_activity_entries
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_activity_none {}",
        task.preempt_guard_activity_none
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_irq_return_entries {}",
        task.preempt_guard_irq_return_entries
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_irq_return_none {}",
        task.preempt_guard_irq_return_none
    )
    .unwrap();
    writeln!(
        output,
        "runtime_irq_guard_entries {}",
        task.runtime_irq_guard_entries
    )
    .unwrap();
    writeln!(
        output,
        "runtime_irq_guard_none {}",
        task.runtime_irq_guard_none
    )
    .unwrap();
    writeln!(
        output,
        "irq_guard_ticket_entries {}",
        task.irq_guard_ticket_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_guard_ticket_none {}",
        task.irq_guard_ticket_none
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_thread_sched_entries {}",
        task.irq_ticket_thread_sched_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_deadline_server_entries {}",
        task.irq_ticket_deadline_server_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_run_queue_entries {}",
        task.irq_ticket_cpu_run_queue_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_run_queue_transaction_entries {}",
        task.irq_ticket_cpu_run_queue_transaction_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_run_queue_owner_observation_entries {}",
        task.irq_ticket_cpu_run_queue_owner_observation_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_run_queue_owner_current_thread_observation_entries {}",
        task.irq_ticket_cpu_run_queue_owner_current_thread_observation_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_run_queue_owner_current_core_observation_entries {}",
        task.irq_ticket_cpu_run_queue_owner_current_core_observation_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_run_queue_owner_runnable_observation_entries {}",
        task.irq_ticket_cpu_run_queue_owner_runnable_observation_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_run_queue_timer_observation_entries {}",
        task.irq_ticket_cpu_run_queue_timer_observation_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_run_queue_timer_deadline_derivation_observation_entries {}",
        task.irq_ticket_cpu_run_queue_timer_deadline_derivation_observation_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_run_queue_rt_accounting_entries {}",
        task.irq_ticket_cpu_run_queue_rt_accounting_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_run_queue_deadline_accounting_entries {}",
        task.irq_ticket_cpu_run_queue_deadline_accounting_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_run_queue_membarrier_entries {}",
        task.irq_ticket_cpu_run_queue_membarrier_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_run_queue_lifecycle_entries {}",
        task.irq_ticket_cpu_run_queue_lifecycle_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_rt_bandwidth_entries {}",
        task.irq_ticket_cpu_rt_bandwidth_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_deadline_entries {}",
        task.irq_ticket_cpu_deadline_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_deadline_observation_entries {}",
        task.irq_ticket_cpu_deadline_observation_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_deadline_publication_entries {}",
        task.irq_ticket_cpu_deadline_publication_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_deadline_registration_entries {}",
        task.irq_ticket_cpu_deadline_registration_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_deadline_hard_expiry_entries {}",
        task.irq_ticket_cpu_deadline_hard_expiry_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_deadline_soft_expiry_entries {}",
        task.irq_ticket_cpu_deadline_soft_expiry_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_deadline_lifecycle_entries {}",
        task.irq_ticket_cpu_deadline_lifecycle_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_root_rt_runtime_entries {}",
        task.irq_ticket_root_rt_runtime_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_root_rt_period_entries {}",
        task.irq_ticket_root_rt_period_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_root_deadline_index_entries {}",
        task.irq_ticket_root_deadline_index_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_guard_explicit_entries {}",
        task.irq_guard_explicit_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_guard_explicit_none {}",
        task.irq_guard_explicit_none
    )
    .unwrap();
    writeln!(
        output,
        "irq_guard_runtime_cpu_entries {}",
        task.irq_guard_runtime_cpu_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_guard_runtime_cpu_none {}",
        task.irq_guard_runtime_cpu_none
    )
    .unwrap();
    writeln!(
        output,
        "irq_guard_executor_entries {}",
        task.irq_guard_executor_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_guard_executor_none {}",
        task.irq_guard_executor_none
    )
    .unwrap();
    writeln!(
        output,
        "owner_rq_irqsave_transactions {}",
        task.owner_rq_irqsave_transactions
    )
    .unwrap();
    writeln!(
        output,
        "owner_rq_scheduler_transactions {}",
        task.owner_rq_scheduler_transactions
    )
    .unwrap();
    writeln!(
        output,
        "owner_rq_bootstrap_transactions {}",
        task.owner_rq_bootstrap_transactions
    )
    .unwrap();
    writeln!(output, "direct_wake_attempts {}", task.direct_wake_attempts).unwrap();
    writeln!(
        output,
        "direct_wake_activations {}",
        task.direct_wake_activations
    )
    .unwrap();
    writeln!(output, "direct_wake_enqueues {}", task.direct_wake_enqueues).unwrap();
    writeln!(
        output,
        "direct_wake_preemptions {}",
        task.direct_wake_preemptions
    )
    .unwrap();
    writeln!(
        output,
        "direct_wake_current_kept {}",
        task.direct_wake_current_kept
    )
    .unwrap();
    writeln!(
        output,
        "direct_wake_queued_candidate_selected {}",
        task.direct_wake_queued_candidate_selected
    )
    .unwrap();
    writeln!(
        output,
        "fair_pick_protected_current {}",
        task.fair_pick_protected_current
    )
    .unwrap();
    writeln!(
        output,
        "fair_wake_wakee_ineligible {}",
        task.fair_wake_wakee_ineligible
    )
    .unwrap();
    writeln!(
        output,
        "fair_wake_current_ineligible {}",
        task.fair_wake_current_ineligible
    )
    .unwrap();
    writeln!(
        output,
        "fair_wake_current_protected {}",
        task.fair_wake_current_protected
    )
    .unwrap();
    writeln!(
        output,
        "fair_wake_deadline_precedes {}",
        task.fair_wake_deadline_precedes
    )
    .unwrap();
    writeln!(
        output,
        "fair_wake_deadline_loses {}",
        task.fair_wake_deadline_loses
    )
    .unwrap();
    writeln!(
        output,
        "fair_sleep_lag_positive {}",
        task.fair_sleep_lag_positive
    )
    .unwrap();
    writeln!(output, "fair_sleep_lag_zero {}", task.fair_sleep_lag_zero).unwrap();
    writeln!(
        output,
        "fair_sleep_lag_negative {}",
        task.fair_sleep_lag_negative
    )
    .unwrap();
    writeln!(
        output,
        "fair_sleep_wake_lag_positive {}",
        task.fair_sleep_wake_lag_positive
    )
    .unwrap();
    writeln!(
        output,
        "fair_sleep_wake_lag_zero {}",
        task.fair_sleep_wake_lag_zero
    )
    .unwrap();
    writeln!(
        output,
        "fair_sleep_wake_lag_negative {}",
        task.fair_sleep_wake_lag_negative
    )
    .unwrap();
    writeln!(
        output,
        "fair_delayed_wake_lag_zero {}",
        task.fair_delayed_wake_lag_zero
    )
    .unwrap();
    writeln!(
        output,
        "fair_delayed_wake_lag_negative {}",
        task.fair_delayed_wake_lag_negative
    )
    .unwrap();
    writeln!(
        output,
        "fair_wake_wakee_debt_total_ns {}",
        task.fair_wake_wakee_debt_total_ns
    )
    .unwrap();
    writeln!(
        output,
        "fair_wake_current_debt_total_ns {}",
        task.fair_wake_current_debt_total_ns
    )
    .unwrap();
    writeln!(
        output,
        "fair_wake_current_credit_total_ns {}",
        task.fair_wake_current_credit_total_ns
    )
    .unwrap();
    writeln!(output, "fair_yield_eligible {}", task.fair_yield_eligible).unwrap();
    writeln!(
        output,
        "fair_yield_ineligible {}",
        task.fair_yield_ineligible
    )
    .unwrap();
    writeln!(
        output,
        "fair_yield_forfeit_total_ns {}",
        task.fair_yield_forfeit_total_ns
    )
    .unwrap();
    writeln!(
        output,
        "fair_yield_debt_total_ns {}",
        task.fair_yield_debt_total_ns
    )
    .unwrap();
    writeln!(
        output,
        "fair_delayed_begin_count {}",
        task.fair_delayed_begin_count
    )
    .unwrap();
    writeln!(
        output,
        "fair_delayed_begin_debt_total_ns {}",
        task.fair_delayed_begin_debt_total_ns
    )
    .unwrap();
    writeln!(
        output,
        "fair_delayed_wake_saved_debt_total_ns {}",
        task.fair_delayed_wake_saved_debt_total_ns
    )
    .unwrap();
    writeln!(
        output,
        "fair_delayed_wake_actual_debt_total_ns {}",
        task.fair_delayed_wake_actual_debt_total_ns
    )
    .unwrap();
    writeln!(
        output,
        "fair_delayed_wake_saved_clamp_count {}",
        task.fair_delayed_wake_saved_clamp_count
    )
    .unwrap();
    writeln!(
        output,
        "task_work_publish_calls {}",
        task.task_work_publish_calls
    )
    .unwrap();
    writeln!(
        output,
        "task_work_publish_edges {}",
        task.task_work_publish_edges
    )
    .unwrap();
    writeln!(
        output,
        "task_work_pending_consumed {}",
        task.task_work_pending_consumed
    )
    .unwrap();
    writeln!(
        output,
        "task_work_reassertions {}",
        task.task_work_reassertions
    )
    .unwrap();
    writeln!(
        output,
        "task_work_worker_passes {}",
        task.task_work_worker_passes
    )
    .unwrap();
    writeln!(
        output,
        "task_work_worker_processed {}",
        task.task_work_worker_processed
    )
    .unwrap();
    writeln!(
        output,
        "task_work_worker_yields {}",
        task.task_work_worker_yields
    )
    .unwrap();
    writeln!(
        output,
        "task_work_worker_waits {}",
        task.task_work_worker_waits
    )
    .unwrap();
    writeln!(
        output,
        "task_work_deadline_events {}",
        task.task_work_deadline_events
    )
    .unwrap();
    writeln!(
        output,
        "task_work_scheduler_tick_events {}",
        task.task_work_scheduler_tick_events
    )
    .unwrap();
    writeln!(
        output,
        "task_work_exit_callbacks {}",
        task.task_work_exit_callbacks
    )
    .unwrap();
    writeln!(
        output,
        "task_work_reaped_threads {}",
        task.task_work_reaped_threads
    )
    .unwrap();
    writeln!(
        output,
        "task_work_coroutine_reclaims {}",
        task.task_work_coroutine_reclaims
    )
    .unwrap();
    writeln!(
        output,
        "task_work_address_space_reclaims {}",
        task.task_work_address_space_reclaims
    )
    .unwrap();
    writeln!(
        output,
        "pi_mutex_lock_attempts {}",
        task.pi_mutex_lock_attempts
    )
    .unwrap();
    writeln!(
        output,
        "pi_mutex_fast_acquisitions {}",
        task.pi_mutex_fast_acquisitions
    )
    .unwrap();
    writeln!(
        output,
        "pi_mutex_slow_entries {}",
        task.pi_mutex_slow_entries
    )
    .unwrap();
    writeln!(
        output,
        "pi_mutex_slow_race_acquisitions {}",
        task.pi_mutex_slow_race_acquisitions
    )
    .unwrap();
    writeln!(
        output,
        "pi_mutex_waiter_registrations {}",
        task.pi_mutex_waiter_registrations
    )
    .unwrap();
    writeln!(
        output,
        "pi_mutex_waiter_parks {}",
        task.pi_mutex_waiter_parks
    )
    .unwrap();
    writeln!(
        output,
        "pi_mutex_contended_releases {}",
        task.pi_mutex_contended_releases
    )
    .unwrap();
    writeln!(
        output,
        "scheduler_ipi_sends {}",
        metrics.scheduler_ipi_sends
    )
    .unwrap();
    writeln!(
        output,
        "scheduler_ipi_consumes {}",
        metrics.scheduler_ipi_consumes
    )
    .unwrap();
    writeln!(output, "clockevent_irqs {}", metrics.clockevent_irqs).unwrap();
    writeln!(output, "context_switches {}", task.context_switches).unwrap();
    writeln!(
        output,
        "context_switches_preempted {}",
        task.context_switches_preempted
    )
    .unwrap();
    writeln!(
        output,
        "context_switches_yield {}",
        task.context_switches_yield
    )
    .unwrap();
    writeln!(
        output,
        "context_switches_blocked {}",
        task.context_switches_blocked
    )
    .unwrap();
    writeln!(
        output,
        "context_switches_exited {}",
        task.context_switches_exited
    )
    .unwrap();
    writeln!(
        output,
        "context_switches_migrated {}",
        task.context_switches_migrated
    )
    .unwrap();
    output
}

#[cfg(all(test, feature = "qperf-metrics"))]
mod tests {
    #[test]
    fn scheduler_metrics_are_machine_readable() {
        let output = super::render_scheduler_metrics();
        let keys = output
            .lines()
            .map(|line| line.split_once(' ').unwrap().0)
            .collect::<alloc::vec::Vec<_>>();

        assert_eq!(
            keys,
            [
                "current_thread_handle_queries",
                "scheduler_deadline_derivation_entries",
                "scheduler_deadline_derivation_clock_event_entries",
                "scheduler_deadline_derivation_park_arm_entries",
                "scheduler_deadline_derivation_park_cancel_entries",
                "scheduler_deadline_derivation_kernel_timer_entries",
                "scheduler_deadline_derivation_ktimer_service_entries",
                "scheduler_deadline_derivation_enqueue_entries",
                "scheduler_deadline_derivation_placement_entries",
                "scheduler_deadline_derivation_schedule_selection_entries",
                "scheduler_deadline_derivation_schedule_no_switch_entries",
                "runtime_preempt_guard_entries",
                "runtime_preempt_guard_none",
                "preempt_guard_ticket_entries",
                "preempt_guard_ticket_none",
                "preempt_guard_explicit_entries",
                "preempt_guard_explicit_none",
                "preempt_guard_sync_entries",
                "preempt_guard_sync_none",
                "preempt_guard_activity_entries",
                "preempt_guard_activity_none",
                "preempt_guard_irq_return_entries",
                "preempt_guard_irq_return_none",
                "runtime_irq_guard_entries",
                "runtime_irq_guard_none",
                "irq_guard_ticket_entries",
                "irq_guard_ticket_none",
                "irq_ticket_thread_sched_entries",
                "irq_ticket_deadline_server_entries",
                "irq_ticket_cpu_run_queue_entries",
                "irq_ticket_cpu_run_queue_transaction_entries",
                "irq_ticket_cpu_run_queue_owner_observation_entries",
                "irq_ticket_cpu_run_queue_owner_current_thread_observation_entries",
                "irq_ticket_cpu_run_queue_owner_current_core_observation_entries",
                "irq_ticket_cpu_run_queue_owner_runnable_observation_entries",
                "irq_ticket_cpu_run_queue_timer_observation_entries",
                "irq_ticket_cpu_run_queue_timer_deadline_derivation_observation_entries",
                "irq_ticket_cpu_run_queue_rt_accounting_entries",
                "irq_ticket_cpu_run_queue_deadline_accounting_entries",
                "irq_ticket_cpu_run_queue_membarrier_entries",
                "irq_ticket_cpu_run_queue_lifecycle_entries",
                "irq_ticket_cpu_rt_bandwidth_entries",
                "irq_ticket_cpu_deadline_entries",
                "irq_ticket_cpu_deadline_observation_entries",
                "irq_ticket_cpu_deadline_publication_entries",
                "irq_ticket_cpu_deadline_registration_entries",
                "irq_ticket_cpu_deadline_hard_expiry_entries",
                "irq_ticket_cpu_deadline_soft_expiry_entries",
                "irq_ticket_cpu_deadline_lifecycle_entries",
                "irq_ticket_root_rt_runtime_entries",
                "irq_ticket_root_rt_period_entries",
                "irq_ticket_root_deadline_index_entries",
                "irq_guard_explicit_entries",
                "irq_guard_explicit_none",
                "irq_guard_runtime_cpu_entries",
                "irq_guard_runtime_cpu_none",
                "irq_guard_executor_entries",
                "irq_guard_executor_none",
                "owner_rq_irqsave_transactions",
                "owner_rq_scheduler_transactions",
                "owner_rq_bootstrap_transactions",
                "direct_wake_attempts",
                "direct_wake_activations",
                "direct_wake_enqueues",
                "direct_wake_preemptions",
                "direct_wake_current_kept",
                "direct_wake_queued_candidate_selected",
                "fair_pick_protected_current",
                "fair_wake_wakee_ineligible",
                "fair_wake_current_ineligible",
                "fair_wake_current_protected",
                "fair_wake_deadline_precedes",
                "fair_wake_deadline_loses",
                "fair_sleep_lag_positive",
                "fair_sleep_lag_zero",
                "fair_sleep_lag_negative",
                "fair_sleep_wake_lag_positive",
                "fair_sleep_wake_lag_zero",
                "fair_sleep_wake_lag_negative",
                "fair_delayed_wake_lag_zero",
                "fair_delayed_wake_lag_negative",
                "fair_wake_wakee_debt_total_ns",
                "fair_wake_current_debt_total_ns",
                "fair_wake_current_credit_total_ns",
                "fair_yield_eligible",
                "fair_yield_ineligible",
                "fair_yield_forfeit_total_ns",
                "fair_yield_debt_total_ns",
                "fair_delayed_begin_count",
                "fair_delayed_begin_debt_total_ns",
                "fair_delayed_wake_saved_debt_total_ns",
                "fair_delayed_wake_actual_debt_total_ns",
                "fair_delayed_wake_saved_clamp_count",
                "task_work_publish_calls",
                "task_work_publish_edges",
                "task_work_pending_consumed",
                "task_work_reassertions",
                "task_work_worker_passes",
                "task_work_worker_processed",
                "task_work_worker_yields",
                "task_work_worker_waits",
                "task_work_deadline_events",
                "task_work_scheduler_tick_events",
                "task_work_exit_callbacks",
                "task_work_reaped_threads",
                "task_work_coroutine_reclaims",
                "task_work_address_space_reclaims",
                "pi_mutex_lock_attempts",
                "pi_mutex_fast_acquisitions",
                "pi_mutex_slow_entries",
                "pi_mutex_slow_race_acquisitions",
                "pi_mutex_waiter_registrations",
                "pi_mutex_waiter_parks",
                "pi_mutex_contended_releases",
                "scheduler_ipi_sends",
                "scheduler_ipi_consumes",
                "clockevent_irqs",
                "context_switches",
                "context_switches_preempted",
                "context_switches_yield",
                "context_switches_blocked",
                "context_switches_exited",
                "context_switches_migrated",
            ]
        );
    }
}
