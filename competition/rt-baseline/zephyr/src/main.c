/* SPDX-License-Identifier: Apache-2.0 */

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include <zephyr/irq.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/atomic.h>
#include <zephyr/sys/poweroff.h>
#include <zephyr/sys/printk.h>
#include <zephyr/sys/util.h>

#include "miss_accounting.h"

#define PERIOD_US 1000U
#define PERIOD_TICKS 1U
#define SAMPLE_COUNT 10000U
#define WARMUP_COUNT 100U
#define TOTAL_EXPIRATIONS (WARMUP_COUNT + SAMPLE_COUNT)
#define START_DELAY_TICKS 2U
#define BENCHMARK_PRIORITY K_PRIO_COOP(0)
#define STRESS_PRIORITY K_PRIO_PREEMPT(5)
#define BENCHMARK_STACK_SIZE 4096U
#define STRESS_STACK_SIZE 2048U
#define STRESS_OPERATIONS_PER_BLOCK 1024U

BUILD_ASSERT(CONFIG_SYS_CLOCK_TICKS_PER_SEC == 1000,
	     "the configured tick must equal the benchmark period");
BUILD_ASSERT(IS_ENABLED(CONFIG_BOARD_QEMU_CORTEX_A53),
	     "the baseline requires qemu_cortex_a53");
BUILD_ASSERT(IS_ENABLED(CONFIG_ARM_ARCH_TIMER),
	     "the baseline requires the AArch64 architected timer");
BUILD_ASSERT(CONFIG_MP_MAX_NUM_CPUS == 1, "the native baseline is single-core");
BUILD_ASSERT(IS_ENABLED(CONFIG_TIMEOUT_64BIT),
	     "absolute periodic deadlines require 64-bit timeouts");
BUILD_ASSERT(!IS_ENABLED(CONFIG_QEMU_ICOUNT),
	     "instruction-count time is not comparable with the AxVisor run");
BUILD_ASSERT(BENCHMARK_PRIORITY < STRESS_PRIORITY,
	     "the benchmark must preempt the stress workload");

#if defined(CONFIG_RT_BASELINE_STRESS)
#define WORKLOAD_NAME "cpu-stress"
#else
#define WORKLOAD_NAME "idle"
#endif

struct latency_summary {
	uint64_t minimum_ns;
	uint64_t mean_ns;
	uint64_t p50_ns;
	uint64_t p90_ns;
	uint64_t p99_ns;
	uint64_t p999_ns;
	uint64_t maximum_ns;
};

struct load_snapshot {
	k_thread_runtime_stats_t cpu;
	k_thread_runtime_stats_t stress;
	k_thread_runtime_stats_t benchmark;
	uint64_t cycle;
	atomic_val_t stress_blocks;
};

K_THREAD_STACK_DEFINE(benchmark_stack, BENCHMARK_STACK_SIZE);
#if defined(CONFIG_RT_BASELINE_STRESS)
K_THREAD_STACK_DEFINE(stress_stack, STRESS_STACK_SIZE);
#endif
K_SEM_DEFINE(timer_semaphore, 0, 1);
K_SEM_DEFINE(completion_semaphore, 0, 1);

static struct k_thread benchmark_thread;
#if defined(CONFIG_RT_BASELINE_STRESS)
static struct k_thread stress_thread;
#endif
static struct k_timer wake_timer;
static uint64_t wake_lateness_ns[SAMPLE_COUNT];
static uint64_t dispatch_latency_ns[SAMPLE_COUNT];
static uint64_t sort_scratch[SAMPLE_COUNT];
static uint64_t timer_expiry_cycles[TOTAL_EXPIRATIONS];
static uint64_t first_deadline_tick;
static uint64_t tick_epoch_cycle;
static uint64_t cycles_per_tick;
static uint64_t measurement_duration_us;
static uint32_t warmup_missed_expirations;
static uint32_t measured_missed_expirations;
static atomic_t timer_expiry_count;
static atomic_t stress_blocks;
#if defined(CONFIG_RT_BASELINE_STRESS)
static volatile uint64_t stress_sink;
#endif
static struct load_snapshot load_start;
static struct load_snapshot load_end;
static bool benchmark_succeeded;

static void benchmark_entry(void *unused1, void *unused2, void *unused3);
#if defined(CONFIG_RT_BASELINE_STRESS)
static void stress_entry(void *unused1, void *unused2, void *unused3);
#endif
static bool capture_load_snapshot(struct load_snapshot *snapshot,
				  k_tid_t benchmark_tid);
static bool start_wake_timer(void);
static void timer_expiry(struct k_timer *unused);
static void benchmark_fail(const char *stage, const char *reason);
static bool emit_latency_result(const char *metric, const uint64_t *samples);
static bool summarize_latencies(const uint64_t *samples,
				struct latency_summary *summary);
static uint64_t nearest_rank(uint32_t numerator, uint32_t denominator);
static int compare_u64(const void *left, const void *right);
static uint64_t delta_u64(uint64_t end, uint64_t start);
static uint64_t ratio_permille(uint64_t portion, uint64_t whole);
static void emit_load_result(void);

int main(void)
{
	const uint32_t clock_hz = sys_clock_hw_cycles_per_sec();
	k_tid_t benchmark_tid;

	if ((clock_hz % CONFIG_SYS_CLOCK_TICKS_PER_SEC) != 0U) {
		printk("RTOS_BASELINE_FATAL schema=1 stage=clock reason=non-integral-tick clock_hz=%u\n",
		       clock_hz);
		return 1;
	}
	cycles_per_tick = clock_hz / CONFIG_SYS_CLOCK_TICKS_PER_SEC;
	printk("RTOS_BASELINE_CONFIG schema=1 os=zephyr zephyr_version=4.3.0 "
	       "board=qemu_cortex_a53 cpu_model=cortex-a53 cpu_count=1 qemu_icount=false "
	       "workload=%s period_us=%u samples=%u warmup=%u benchmark_priority=%d "
	       "stress_priority=%d clock_hz=%u ticks_per_sec=%u\n",
	       WORKLOAD_NAME, PERIOD_US, SAMPLE_COUNT, WARMUP_COUNT,
	       BENCHMARK_PRIORITY, STRESS_PRIORITY, clock_hz,
	       CONFIG_SYS_CLOCK_TICKS_PER_SEC);

#if defined(CONFIG_RT_BASELINE_STRESS)
	k_tid_t stress_tid = k_thread_create(
		&stress_thread, stress_stack, K_THREAD_STACK_SIZEOF(stress_stack),
		stress_entry, NULL, NULL, NULL, STRESS_PRIORITY, 0, K_NO_WAIT);

	k_thread_name_set(stress_tid, "rt-stress");
	k_sleep(K_MSEC(10));
	if (atomic_get(&stress_blocks) <= 0 ||
	    k_thread_priority_get(stress_tid) != STRESS_PRIORITY) {
		printk("RTOS_BASELINE_FATAL schema=1 stage=workload reason=stress-not-running\n");
		return 1;
	}
	printk("RTOS_BASELINE_WORKLOAD_READY schema=1 kind=cpu-stress verified=true "
	       "lower_priority=true benchmark_priority=%d stress_priority=%d blocks=%ld\n",
	       BENCHMARK_PRIORITY, STRESS_PRIORITY, (long)atomic_get(&stress_blocks));
#else
	printk("RTOS_BASELINE_WORKLOAD_READY schema=1 kind=idle verified=true "
	       "lower_priority=false benchmark_priority=%d stress_priority=%d blocks=0\n",
	       BENCHMARK_PRIORITY, STRESS_PRIORITY);
#endif

	if (!capture_load_snapshot(&load_start, NULL)) {
		printk("RTOS_BASELINE_FATAL schema=1 stage=load-snapshot reason=start-failed\n");
		return 1;
	}
	benchmark_tid = k_thread_create(
		&benchmark_thread, benchmark_stack,
		K_THREAD_STACK_SIZEOF(benchmark_stack), benchmark_entry, NULL, NULL,
		NULL, BENCHMARK_PRIORITY, 0, K_NO_WAIT);
	k_thread_name_set(benchmark_tid, "rt-periodic");
	k_sem_take(&completion_semaphore, K_FOREVER);

	if (!benchmark_succeeded) {
		return 1;
	}
	emit_load_result();
	printk("RTOS_BASELINE_COMPLETE schema=1 workload=%s status=pass "
	       "timer_misses=%u warmup_timer_misses=%u early_wakes=0\n",
	       WORKLOAD_NAME, measured_missed_expirations,
	       warmup_missed_expirations);
	sys_poweroff();
	return 0;
}

static bool capture_load_snapshot(struct load_snapshot *snapshot,
				  k_tid_t benchmark_tid)
{
	if (k_thread_runtime_stats_cpu_get(0, &snapshot->cpu) != 0) {
		return false;
	}
#if defined(CONFIG_RT_BASELINE_STRESS)
	if (k_thread_runtime_stats_get(&stress_thread, &snapshot->stress) != 0) {
		return false;
	}
#else
	snapshot->stress = (k_thread_runtime_stats_t){};
#endif
	if (benchmark_tid != NULL) {
		if (k_thread_runtime_stats_get(benchmark_tid, &snapshot->benchmark) != 0) {
			return false;
		}
	} else {
		snapshot->benchmark = (k_thread_runtime_stats_t){};
	}
	snapshot->cycle = k_cycle_get_64();
	snapshot->stress_blocks = atomic_get(&stress_blocks);
	return true;
}

static void timer_expiry(struct k_timer *unused)
{
	const atomic_val_t expiry = atomic_inc(&timer_expiry_count);

	if (expiry >= 0 && expiry < (atomic_val_t)TOTAL_EXPIRATIONS) {
		timer_expiry_cycles[expiry] = k_cycle_get_64();
	}
	if (expiry + 1 >= (atomic_val_t)TOTAL_EXPIRATIONS) {
		k_timer_stop(unused);
	}
	k_sem_give(&timer_semaphore);
}

static bool start_wake_timer(void)
{
	for (uint32_t attempt = 0U; attempt < 3U; ++attempt) {
		uint64_t cycle;
		int64_t tick_before;
		int64_t tick_after;
		unsigned int irq_key = irq_lock();

		tick_before = sys_clock_tick_get();
		cycle = k_cycle_get_64();
		tick_after = sys_clock_tick_get();
		irq_unlock(irq_key);
		if (tick_before != tick_after || tick_before < 0) {
			continue;
		}
		const uint64_t absolute_cycle_tick = cycle / cycles_per_tick;

		if (absolute_cycle_tick < (uint64_t)tick_before) {
			return false;
		}
		tick_epoch_cycle =
			(absolute_cycle_tick - (uint64_t)tick_before) * cycles_per_tick;
		first_deadline_tick = (uint64_t)tick_before + START_DELAY_TICKS;
		k_timer_start(&wake_timer, K_TIMEOUT_ABS_TICKS(first_deadline_tick),
			      K_TICKS(PERIOD_TICKS));
		return true;
	}
	return false;
}

static void benchmark_entry(void *unused1, void *unused2, void *unused3)
{
	uint64_t last_observed_cycle = 0U;
	uint32_t processed_expirations = 0U;

	ARG_UNUSED(unused1);
	ARG_UNUSED(unused2);
	ARG_UNUSED(unused3);
	k_timer_init(&wake_timer, timer_expiry, NULL);
	if (!start_wake_timer()) {
		benchmark_fail("timer-schedule", "unstable-clock-epoch");
		return;
	}

	while (processed_expirations < TOTAL_EXPIRATIONS) {
		uint64_t observed_cycle;
		atomic_val_t available_expirations;
		unsigned int irq_key;

		k_sem_take(&timer_semaphore, K_FOREVER);
		irq_key = irq_lock();
		available_expirations = atomic_get(&timer_expiry_count);
		observed_cycle = k_cycle_get_64();
		irq_unlock(irq_key);
		if (available_expirations <= (atomic_val_t)processed_expirations) {
			continue;
		}
		if (available_expirations > (atomic_val_t)TOTAL_EXPIRATIONS) {
			benchmark_fail("timer-expiration", "invalid-expiry-count");
			return;
		}
		const struct coalesced_expirations coalesced =
			count_coalesced_expirations(
				processed_expirations,
				(uint32_t)available_expirations, WARMUP_COUNT);

		warmup_missed_expirations += coalesced.warmup;
		measured_missed_expirations += coalesced.measured;

		while (processed_expirations < (uint32_t)available_expirations) {
			const uint64_t expected_cycle =
				tick_epoch_cycle +
				(first_deadline_tick +
				 (uint64_t)processed_expirations * PERIOD_TICKS) *
					cycles_per_tick;
			const uint64_t expiry_cycle =
				timer_expiry_cycles[processed_expirations];

			if (expiry_cycle < expected_cycle || observed_cycle < expiry_cycle) {
				benchmark_fail("timestamp", "non-monotonic-or-early");
				return;
			}
			if (processed_expirations >= WARMUP_COUNT) {
				const uint32_t sample =
					processed_expirations - WARMUP_COUNT;

				wake_lateness_ns[sample] = k_cyc_to_ns_floor64(
					observed_cycle - expected_cycle);
				dispatch_latency_ns[sample] = k_cyc_to_ns_floor64(
					observed_cycle - expiry_cycle);
			}
			++processed_expirations;
		}
		last_observed_cycle = observed_cycle;
	}
	k_timer_stop(&wake_timer);

	const uint64_t measurement_start_cycle =
		tick_epoch_cycle +
		(first_deadline_tick + WARMUP_COUNT - PERIOD_TICKS) * cycles_per_tick;

	measurement_duration_us =
		k_cyc_to_us_floor64(last_observed_cycle - measurement_start_cycle);
	if (!capture_load_snapshot(&load_end, &benchmark_thread)) {
		benchmark_fail("load-snapshot", "end-failed");
		return;
	}
	if (!emit_latency_result("periodic_wake_lateness", wake_lateness_ns) ||
	    !emit_latency_result("timer_to_task_dispatch", dispatch_latency_ns)) {
		benchmark_fail("statistics", "overflow-or-invalid");
		return;
	}
	benchmark_succeeded = true;
	k_sem_give(&completion_semaphore);
}

static void benchmark_fail(const char *stage, const char *reason)
{
	k_timer_stop(&wake_timer);
	printk("RTOS_BASELINE_FATAL schema=1 stage=%s reason=%s\n", stage, reason);
	benchmark_succeeded = false;
	k_sem_give(&completion_semaphore);
}

static bool emit_latency_result(const char *metric, const uint64_t *samples)
{
	struct latency_summary summary;

	if (!summarize_latencies(samples, &summary)) {
		return false;
	}
	printk("RTOS_BASELINE_RESULT schema=1 workload=%s metric=%s unit=ns count=%u "
	       "min_ns=%llu mean_ns=%llu p50_ns=%llu p90_ns=%llu p99_ns=%llu "
	       "p999_ns=%llu max_ns=%llu actual_duration_us=%llu "
	       "expected_duration_us=%llu\n",
	       WORKLOAD_NAME, metric, SAMPLE_COUNT,
	       (unsigned long long)summary.minimum_ns,
	       (unsigned long long)summary.mean_ns,
	       (unsigned long long)summary.p50_ns,
	       (unsigned long long)summary.p90_ns,
	       (unsigned long long)summary.p99_ns,
	       (unsigned long long)summary.p999_ns,
	       (unsigned long long)summary.maximum_ns,
	       (unsigned long long)measurement_duration_us,
	       (unsigned long long)SAMPLE_COUNT * PERIOD_US);
	return true;
}

static bool summarize_latencies(const uint64_t *samples,
				struct latency_summary *summary)
{
	uint64_t sum = 0U;

	memcpy(sort_scratch, samples, sizeof(sort_scratch));
	qsort(sort_scratch, SAMPLE_COUNT, sizeof(sort_scratch[0]), compare_u64);
	for (uint32_t index = 0U; index < SAMPLE_COUNT; ++index) {
		if (UINT64_MAX - sum < sort_scratch[index]) {
			return false;
		}
		sum += sort_scratch[index];
	}
	*summary = (struct latency_summary){
		.minimum_ns = sort_scratch[0],
		.mean_ns = sum / SAMPLE_COUNT,
		.p50_ns = nearest_rank(50U, 100U),
		.p90_ns = nearest_rank(90U, 100U),
		.p99_ns = nearest_rank(99U, 100U),
		.p999_ns = nearest_rank(999U, 1000U),
		.maximum_ns = sort_scratch[SAMPLE_COUNT - 1U],
	};
	return true;
}

static uint64_t nearest_rank(uint32_t numerator, uint32_t denominator)
{
	const uint64_t rank =
		((uint64_t)SAMPLE_COUNT * numerator + denominator - 1U) /
		denominator;

	return sort_scratch[rank - 1U];
}

static int compare_u64(const void *left, const void *right)
{
	const uint64_t left_value = *(const uint64_t *)left;
	const uint64_t right_value = *(const uint64_t *)right;

	return (left_value > right_value) - (left_value < right_value);
}

static uint64_t delta_u64(uint64_t end, uint64_t start)
{
	return end >= start ? end - start : 0U;
}

static uint64_t ratio_permille(uint64_t portion, uint64_t whole)
{
	if (whole == 0U || portion > UINT64_MAX / 1000U) {
		return 0U;
	}
	return portion * 1000U / whole;
}

static void emit_load_result(void)
{
	const uint64_t cpu_cycles = delta_u64(load_end.cpu.execution_cycles,
					      load_start.cpu.execution_cycles);
	const uint64_t non_idle_cycles = delta_u64(load_end.cpu.total_cycles,
						   load_start.cpu.total_cycles);
	const uint64_t idle_cycles = delta_u64(load_end.cpu.idle_cycles,
					       load_start.cpu.idle_cycles);
	const uint64_t stress_cycles = delta_u64(load_end.stress.execution_cycles,
						 load_start.stress.execution_cycles);
	const uint64_t benchmark_cycles = delta_u64(
		load_end.benchmark.execution_cycles,
		load_start.benchmark.execution_cycles);
	const uint64_t window_us =
		k_cyc_to_us_floor64(delta_u64(load_end.cycle, load_start.cycle));
	const uint64_t stress_block_delta =
		(uint64_t)(load_end.stress_blocks - load_start.stress_blocks);
	const uint64_t stress_blocks_per_second =
		window_us == 0U || stress_block_delta > UINT64_MAX / 1000000U
			? 0U
			: stress_block_delta * 1000000U / window_us;
	bool verified = cpu_cycles > 0U;

#if defined(CONFIG_RT_BASELINE_STRESS)
	verified = verified && stress_cycles > 0U && stress_block_delta > 0U;
#else
	verified = verified && stress_cycles == 0U && stress_block_delta == 0U;
#endif
	printk("RTOS_BASELINE_LOAD schema=1 workload=%s verified=%s "
	       "window_duration_us=%llu cpu_non_idle_permille=%llu cpu_idle_permille=%llu "
	       "benchmark_permille=%llu stress_permille=%llu stress_blocks=%llu "
	       "stress_blocks_per_second=%llu\n",
	       WORKLOAD_NAME, verified ? "true" : "false",
	       (unsigned long long)window_us,
	       (unsigned long long)ratio_permille(non_idle_cycles, cpu_cycles),
	       (unsigned long long)ratio_permille(idle_cycles, cpu_cycles),
	       (unsigned long long)ratio_permille(benchmark_cycles, cpu_cycles),
	       (unsigned long long)ratio_permille(stress_cycles, cpu_cycles),
	       (unsigned long long)stress_block_delta,
	       (unsigned long long)stress_blocks_per_second);
}

#if defined(CONFIG_RT_BASELINE_STRESS)
static void stress_entry(void *unused1, void *unused2, void *unused3)
{
	uint64_t state = UINT64_C(0x9e3779b97f4a7c15);

	ARG_UNUSED(unused1);
	ARG_UNUSED(unused2);
	ARG_UNUSED(unused3);
	for (;;) {
		for (uint32_t operation = 0U;
		     operation < STRESS_OPERATIONS_PER_BLOCK; ++operation) {
			state ^= state << 13;
			state ^= state >> 7;
			state ^= state << 17;
		}
		stress_sink = state;
		atomic_inc(&stress_blocks);
	}
}
#endif
