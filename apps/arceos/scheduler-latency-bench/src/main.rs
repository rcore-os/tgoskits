use std::{
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

#[cfg(any(feature = "arceos", feature = "qperf-metrics"))]
use ax_std as _;

const THREADS: usize = 2;
const WARMUP_YIELDS: usize = 2_000;
const MEASURED_YIELDS: usize = 20_000;
const ROUNDS: usize = 7;
const FIFO_PRIORITY: u8 = 80;

#[cfg(feature = "qperf-metrics")]
fn metric_average(total: u64, count: u64) -> u64 {
    total.checked_div(count).unwrap_or(0)
}

fn fifo_policy() -> ax_task::SchedulePolicy {
    ax_task::SchedulePolicy::fifo(
        ax_task::RtPriority::new(FIFO_PRIORITY).expect("benchmark FIFO priority must be valid"),
    )
}

fn cpu0_affinity() -> ax_task::CpuSet {
    let mut affinity = ax_task::CpuSet::empty(1);
    assert!(affinity.insert(ax_task::CpuId::new(0)));
    affinity
}

#[inline(always)]
fn scheduler_yield() {
    ax_task::yield_current_cpu().expect("kernel scheduler yield failed");
}

fn spawn_fifo(name: &str, entry: impl FnOnce() + Send + 'static) -> ax_task::KernelThreadHandle {
    ax_task::ThreadBuilder::new(name.into())
        .policy(fifo_policy())
        .affinity(cpu0_affinity())
        .spawn(entry)
        .expect("failed to spawn FIFO benchmark thread")
}

fn run_single_thread_round() -> u128 {
    let start = Arc::new(Barrier::new(2));
    let result = Arc::new(AtomicUsize::new(0));
    let worker_start = Arc::clone(&start);
    let worker_result = Arc::clone(&result);
    let worker = spawn_fifo("fifo-yield-no-peer", move || {
        worker_start.wait();
        for _ in 0..WARMUP_YIELDS {
            scheduler_yield();
        }
        let started = Instant::now();
        for _ in 0..MEASURED_YIELDS {
            scheduler_yield();
        }
        worker_result.store(
            (started.elapsed().as_nanos() / MEASURED_YIELDS as u128) as usize,
            Ordering::Release,
        );
    });
    start.wait();
    worker.join().expect("FIFO no-peer worker failed");
    result.load(Ordering::Acquire) as u128
}

fn run_worker(start: Arc<Barrier>, measure: Arc<Barrier>, result: Arc<AtomicUsize>) {
    start.wait();
    for _ in 0..WARMUP_YIELDS {
        scheduler_yield();
    }
    measure.wait();

    let started = Instant::now();
    for _ in 0..MEASURED_YIELDS {
        scheduler_yield();
    }
    result.store(started.elapsed().as_nanos() as usize, Ordering::Release);
}

fn run_round() -> u128 {
    let start = Arc::new(Barrier::new(THREADS + 1));
    let measure = Arc::new(Barrier::new(THREADS));
    let mut workers = Vec::with_capacity(THREADS);
    let mut results = Vec::with_capacity(THREADS);

    for worker_index in 0..THREADS {
        let start = Arc::clone(&start);
        let measure = Arc::clone(&measure);
        let result = Arc::new(AtomicUsize::new(0));
        let worker_result = Arc::clone(&result);
        workers.push(spawn_fifo(
            if worker_index == 0 {
                "fifo-yield-a"
            } else {
                "fifo-yield-b"
            },
            move || run_worker(start, measure, worker_result),
        ));
        results.push(result);
    }

    start.wait();
    for worker in workers {
        worker.join().expect("FIFO handoff worker failed");
    }
    let elapsed = results
        .into_iter()
        .map(|result| result.load(Ordering::Acquire) as u128)
        .max()
        .expect("scheduler benchmark requires results");
    elapsed / (THREADS * MEASURED_YIELDS) as u128
}

fn main() {
    let mut no_switch_samples = Vec::with_capacity(ROUNDS);
    for round in 0..ROUNDS {
        let nanoseconds = run_single_thread_round();
        println!(
            "kernel_thread_yield_no_switch round={} p=ns_per_yield value={nanoseconds}",
            round + 1
        );
        no_switch_samples.push(nanoseconds);
    }
    no_switch_samples.sort_unstable();
    let no_switch_median = no_switch_samples[ROUNDS / 2];
    println!("kernel_thread_yield_no_switch p50_ns={no_switch_median}");

    #[cfg(feature = "qperf-metrics")]
    let metrics_before = ax_task::qperf_scheduler_metrics_snapshot();
    let mut samples = Vec::with_capacity(ROUNDS);
    for round in 0..ROUNDS {
        let nanoseconds = run_round();
        println!(
            "kernel_thread_switch round={} p=ns_per_switch value={nanoseconds}",
            round + 1
        );
        samples.push(nanoseconds);
    }
    samples.sort_unstable();
    let median = samples[ROUNDS / 2];
    #[cfg(feature = "qperf-metrics")]
    let metrics_after = ax_task::qperf_scheduler_metrics_snapshot();
    println!("kernel_thread_switch p50_ns={median}");
    println!(
        "kernel_thread_switch_increment p50_ns={}",
        median.saturating_sub(no_switch_median),
    );
    #[cfg(feature = "qperf-metrics")]
    println!(
        "kernel_thread_scheduler_metrics context_switches={} owner_rq_scheduler={} \
         owner_rq_irqsave={} runtime_cpu_owner_claims={} current_thread_queries={} \
         preempt_guards={} preempt_guard_none={} irq_guards={} irq_guard_none={} \
         deadline_derivations={} deadline_clock_event={} deadline_enqueue={} \
         deadline_placement={} deadline_selection={} deadline_no_switch={} rq_ticket={} \
         deadline_observation_ticket={} deadline_publication_ticket={}",
        metrics_after.context_switches - metrics_before.context_switches,
        metrics_after.owner_rq_scheduler_transactions
            - metrics_before.owner_rq_scheduler_transactions,
        metrics_after.owner_rq_irqsave_transactions - metrics_before.owner_rq_irqsave_transactions,
        metrics_after.runtime_cpu_owner_claims - metrics_before.runtime_cpu_owner_claims,
        metrics_after.current_thread_handle_queries - metrics_before.current_thread_handle_queries,
        metrics_after.runtime_preempt_guard_entries - metrics_before.runtime_preempt_guard_entries,
        metrics_after.runtime_preempt_guard_none - metrics_before.runtime_preempt_guard_none,
        metrics_after.runtime_irq_guard_entries - metrics_before.runtime_irq_guard_entries,
        metrics_after.runtime_irq_guard_none - metrics_before.runtime_irq_guard_none,
        metrics_after.scheduler_deadline_derivation_entries
            - metrics_before.scheduler_deadline_derivation_entries,
        metrics_after.scheduler_deadline_derivation_clock_event_entries
            - metrics_before.scheduler_deadline_derivation_clock_event_entries,
        metrics_after.scheduler_deadline_derivation_enqueue_entries
            - metrics_before.scheduler_deadline_derivation_enqueue_entries,
        metrics_after.scheduler_deadline_derivation_placement_entries
            - metrics_before.scheduler_deadline_derivation_placement_entries,
        metrics_after.scheduler_deadline_derivation_schedule_selection_entries
            - metrics_before.scheduler_deadline_derivation_schedule_selection_entries,
        metrics_after.scheduler_deadline_derivation_schedule_no_switch_entries
            - metrics_before.scheduler_deadline_derivation_schedule_no_switch_entries,
        metrics_after.irq_ticket_cpu_run_queue_transaction_entries
            - metrics_before.irq_ticket_cpu_run_queue_transaction_entries,
        metrics_after.irq_ticket_cpu_deadline_observation_entries
            - metrics_before.irq_ticket_cpu_deadline_observation_entries,
        metrics_after.irq_ticket_cpu_deadline_publication_entries
            - metrics_before.irq_ticket_cpu_deadline_publication_entries,
    );
    #[cfg(feature = "qperf-metrics")]
    {
        let scheduler_count = metrics_after.switch_phase_scheduler_count
            - metrics_before.switch_phase_scheduler_count;
        let scheduler_total = metrics_after.switch_phase_scheduler_total_ns
            - metrics_before.switch_phase_scheduler_total_ns;
        let prepare_count =
            metrics_after.switch_phase_prepare_count - metrics_before.switch_phase_prepare_count;
        let prepare_total = metrics_after.switch_phase_prepare_total_ns
            - metrics_before.switch_phase_prepare_total_ns;
        let runtime_tail_count = metrics_after.switch_phase_runtime_tail_count
            - metrics_before.switch_phase_runtime_tail_count;
        let runtime_tail_total = metrics_after.switch_phase_runtime_tail_total_ns
            - metrics_before.switch_phase_runtime_tail_total_ns;
        let owner_tail_count = metrics_after.switch_phase_owner_tail_count
            - metrics_before.switch_phase_owner_tail_count;
        let owner_tail_total = metrics_after.switch_phase_owner_tail_total_ns
            - metrics_before.switch_phase_owner_tail_total_ns;
        println!(
            "kernel_thread_switch_phases scheduler_count={} scheduler_avg_ns={} prepare_count={} \
             prepare_avg_ns={} runtime_tail_count={} runtime_tail_avg_ns={} owner_tail_count={} \
             owner_tail_avg_ns={}",
            scheduler_count,
            metric_average(scheduler_total, scheduler_count),
            prepare_count,
            metric_average(prepare_total, prepare_count),
            runtime_tail_count,
            metric_average(runtime_tail_total, runtime_tail_count),
            owner_tail_count,
            metric_average(owner_tail_total, owner_tail_count),
        );
        let mut detail_count = [0_u64; 10];
        let mut detail_average = [0_u64; 10];
        for index in 0..10 {
            detail_count[index] = metrics_after.switch_scheduler_detail_count[index]
                - metrics_before.switch_scheduler_detail_count[index];
            let detail_total = metrics_after.switch_scheduler_detail_total_ns[index]
                - metrics_before.switch_scheduler_detail_total_ns[index];
            detail_average[index] = metric_average(detail_total, detail_count[index]);
        }
        println!(
            "kernel_thread_scheduler_detail account_count={} account_avg_ns={} put_prev_count={} \
             put_prev_avg_ns={} pick_count={} pick_avg_ns={} handoff_count={} handoff_avg_ns={} \
             rq_commit_count={} rq_commit_avg_ns={} dispatch_count={} dispatch_avg_ns={} \
             selection_tail_count={} selection_tail_avg_ns={} frame_enter_count={} \
             frame_enter_avg_ns={} facade_setup_count={} facade_setup_avg_ns={} \
             owner_schedule_count={} owner_schedule_avg_ns={}",
            detail_count[0],
            detail_average[0],
            detail_count[1],
            detail_average[1],
            detail_count[2],
            detail_average[2],
            detail_count[3],
            detail_average[3],
            detail_count[4],
            detail_average[4],
            detail_count[5],
            detail_average[5],
            detail_count[6],
            detail_average[6],
            detail_count[7],
            detail_average[7],
            detail_count[8],
            detail_average[8],
            detail_count[9],
            detail_average[9],
        );
    }
    println!("KERNEL_SCHED_BENCH_PASSED");
}
