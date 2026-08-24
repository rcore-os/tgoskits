#include <stdint.h>

#include <zephyr/device.h>
#include <zephyr/devicetree.h>
#include <zephyr/drivers/uart.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/time_units.h>

#define PERIOD_MS 10
#ifndef RT_SAMPLE_COUNT
#define RT_SAMPLE_COUNT 300
#endif
#define SAMPLE_COUNT RT_SAMPLE_COUNT

#ifndef RT_START_DELAY_MS
#define RT_START_DELAY_MS 0
#endif

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

static void print_samples(const struct latency_sample *captured)
{
	printk("sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns\n");
	for (int64_t sequence = 0; sequence < SAMPLE_COUNT; sequence++) {
		printk("%lld,%lld,%lld,%lld,%lld\n", sequence,
		       captured[sequence].timestamp_ns,
		       captured[sequence].deadline_ns,
		       captured[sequence].actual_ns,
		       captured[sequence].jitter_ns);
	}
}

#ifdef RT_START_GATED
static void wait_for_console_byte(unsigned char expected)
{
	const struct device *console = DEVICE_DT_GET(DT_CHOSEN(zephyr_console));
	unsigned char byte = 0;

	if (!device_is_ready(console)) {
		printk("PERIODIC LATENCY ERROR console-not-ready\n");
		return;
	}

	while (byte != expected) {
		if (uart_poll_in(console, &byte) != 0) {
			k_sleep(K_MSEC(1));
		}
	}
}

static void wait_for_start(void)
{
	printk("PERIODIC LATENCY READY\n");
	wait_for_console_byte('g');
	printk("PERIODIC LATENCY START\n");
}
#endif

#if defined(RT_DUMP_GATED) && !defined(RT_START_GATED)
#error "RT_DUMP_GATED requires RT_START_GATED for console input support"
#endif

int main(void)
{
	const int64_t period_ticks = k_ms_to_ticks_ceil64(PERIOD_MS);
	const int64_t period_cycles =
		(int64_t)sys_clock_hw_cycles_per_sec() * PERIOD_MS / 1000;
	int64_t base_ticks;

#ifdef RT_START_GATED
	wait_for_start();
#endif
	if (RT_START_DELAY_MS > 0) {
		printk("PERIODIC LATENCY SETTLE delay_ms=%d\n", RT_START_DELAY_MS);
		k_sleep(K_MSEC(RT_START_DELAY_MS));
	}

	base_ticks = k_uptime_ticks();

	while (k_uptime_ticks() == base_ticks) {
	}
	base_ticks = k_uptime_ticks();
	const int64_t base_cycles = (int64_t)k_cycle_get_64();

	for (int64_t sequence = 0; sequence < SAMPLE_COUNT; sequence++) {
		const int64_t deadline_ticks =
			base_ticks + (sequence + 1) * period_ticks;

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

	#ifdef RT_DUMP_GATED
	printk("PERIODIC LATENCY SAMPLING COMPLETE samples=%d\n", SAMPLE_COUNT);
	wait_for_console_byte('d');
	#endif
	print_samples(samples);
	printk("PERIODIC LATENCY COMPLETE samples=%d\n", SAMPLE_COUNT);
	return 0;
}
