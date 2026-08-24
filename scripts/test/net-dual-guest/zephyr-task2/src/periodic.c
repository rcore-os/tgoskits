/*
 * Task-1 periodic wake-up probe embedded in the Task-2/3 Zephyr endpoint.
 *
 * The probe is dormant until the board runner sends 'g' to the Zephyr
 * console. It retains samples in RAM and waits for 'd' before exporting
 * them, so UART output cannot perturb the measured interval. The network
 * endpoint in main.c continues to run while this thread samples.
 */

#include <stdint.h>

#include <zephyr/device.h>
#include <zephyr/devicetree.h>
#include <zephyr/drivers/uart.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/time_units.h>

#include "console.h"
#include "telemetry.h"

#define PERIOD_MS 10
#ifndef RT_SAMPLE_COUNT
#define RT_SAMPLE_COUNT 300
#endif
#define SAMPLE_COUNT RT_SAMPLE_COUNT

struct latency_sample {
	int64_t timestamp_ns;
	int64_t deadline_ns;
	int64_t actual_ns;
	int64_t jitter_ns;
};

static struct latency_sample samples[SAMPLE_COUNT];

static int64_t cycles_to_ns(int64_t cycles)
{
	return (int64_t)k_cyc_to_ns_floor64((uint64_t)cycles);
}

static void wait_for_console_byte(unsigned char expected)
{
	const struct device *console = DEVICE_DT_GET(DT_CHOSEN(zephyr_console));
	unsigned char byte = 0;

	while (!device_is_ready(console)) {
		k_sleep(K_MSEC(1));
	}

	while (byte != expected) {
		if (uart_poll_in(console, &byte) != 0) {
			k_sleep(K_MSEC(1));
		}
	}
}

static void capture_samples(void)
{
	const int64_t period_ticks = k_ms_to_ticks_ceil64(PERIOD_MS);
	const int64_t period_cycles =
		(int64_t)sys_clock_hw_cycles_per_sec() * PERIOD_MS / 1000;
	int64_t base_ticks = k_uptime_ticks();

	while (k_uptime_ticks() == base_ticks) {
	}
	base_ticks = k_uptime_ticks();
	const int64_t base_cycles = (int64_t)k_cycle_get_64();

	for (int64_t sequence = 0; sequence < SAMPLE_COUNT; sequence++) {
		const int64_t deadline_ticks = base_ticks + (sequence + 1) * period_ticks;

		k_sleep(K_TIMEOUT_ABS_TICKS(deadline_ticks));

		const int64_t actual_cycles = (int64_t)k_cycle_get_64();
		const int64_t deadline_ns =
			cycles_to_ns(base_cycles + (sequence + 1) * period_cycles);
		const int64_t actual_ns = cycles_to_ns(actual_cycles);
		samples[sequence] = (struct latency_sample) {
			.timestamp_ns = cycles_to_ns(actual_cycles - base_cycles),
			.deadline_ns = deadline_ns,
			.actual_ns = actual_ns,
			.jitter_ns = actual_ns - deadline_ns,
		};
	}
}

static void print_samples(void)
{
	task2_console_lock();
	task2_console_printf_locked("sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns\n");
	for (int64_t sequence = 0; sequence < SAMPLE_COUNT; sequence++) {
		task2_console_printf_locked("%lld,%lld,%lld,%lld,%lld\n", sequence,
					    samples[sequence].timestamp_ns,
					    samples[sequence].deadline_ns,
					    samples[sequence].actual_ns,
					    samples[sequence].jitter_ns);
	}
	task2_console_printf_locked("PERIODIC LATENCY COMPLETE samples=%d\n", SAMPLE_COUNT);
	task2_console_unlock();
}

static void periodic_probe_thread(void *unused1, void *unused2, void *unused3)
{
	struct task2_telemetry_snapshot telemetry_before;
	struct task2_telemetry_snapshot telemetry_after;

	ARG_UNUSED(unused1);
	ARG_UNUSED(unused2);
	ARG_UNUSED(unused3);

	task2_console_printf(
		"PERIODIC LATENCY READY frequency_hz=%u period_ms=%u samples=%u\n",
		(unsigned int)sys_clock_hw_cycles_per_sec(), PERIOD_MS, SAMPLE_COUNT);
	wait_for_console_byte('g');
	task2_console_printf("PERIODIC LATENCY START\n");
	task2_console_set_trace_quiet(true);
	telemetry_before = task2_telemetry_snapshot();
	capture_samples();
	telemetry_after = task2_telemetry_snapshot();
	task2_console_set_trace_quiet(false);
	task2_console_printf(
		"PERIODIC LATENCY SAMPLING COMPLETE samples=%d controls=%u statuses=%u heartbeats=%u\n",
		SAMPLE_COUNT,
		telemetry_after.controls_received - telemetry_before.controls_received,
		telemetry_after.statuses_sent - telemetry_before.statuses_sent,
		telemetry_after.heartbeats_received - telemetry_before.heartbeats_received);
	wait_for_console_byte('d');
	print_samples();
}

K_THREAD_DEFINE(periodic_probe_thread_id, 2048, periodic_probe_thread, NULL, NULL, NULL,
		0, 0, 0);
