#include <stdint.h>

#include <zephyr/kernel.h>
#include <zephyr/sys/time_units.h>

#define PERIOD_MS 10
#define SAMPLE_COUNT 300

static int64_t cycles_to_ns(int64_t cycles)
{
	return (int64_t)k_cyc_to_ns_floor64((uint64_t)cycles);
}

int main(void)
{
	const int64_t period_ticks = k_ms_to_ticks_ceil64(PERIOD_MS);
	const int64_t period_cycles =
		(int64_t)sys_clock_hw_cycles_per_sec() * PERIOD_MS / 1000;
	int64_t base_ticks = k_uptime_ticks();

	while (k_uptime_ticks() == base_ticks) {
	}
	base_ticks = k_uptime_ticks();
	const int64_t base_cycles = (int64_t)k_cycle_get_64();

	printk("sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns\n");
	for (int64_t sequence = 0; sequence < SAMPLE_COUNT; sequence++) {
		const int64_t deadline_ticks =
			base_ticks + (sequence + 1) * period_ticks;

		k_sleep(K_TIMEOUT_ABS_TICKS(deadline_ticks));

		const int64_t actual_cycles = (int64_t)k_cycle_get_64();
		const int64_t deadline_ns =
			cycles_to_ns(base_cycles + (sequence + 1) * period_cycles);
		const int64_t actual_ns = cycles_to_ns(actual_cycles);
		printk("%lld,%lld,%lld,%lld,%lld\n", sequence,
		       cycles_to_ns(actual_cycles - base_cycles), deadline_ns,
		       actual_ns, actual_ns - deadline_ns);
	}

	printk("PERIODIC LATENCY COMPLETE samples=%d\n", SAMPLE_COUNT);
	return 0;
}
