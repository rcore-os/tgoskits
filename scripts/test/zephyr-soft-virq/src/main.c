#include <stdint.h>

#include <zephyr/irq.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/time_units.h>

#define SOFTWARE_VIRQ 48
#define SAMPLE_COUNT 300
#define WAIT_TIMEOUT_MS 10000

static volatile uint32_t virq_count;
static volatile uint64_t virq_cycles[SAMPLE_COUNT];

static void software_virq_isr(const void *arg)
{
	ARG_UNUSED(arg);

	const uint32_t sequence = virq_count;
	if (sequence < SAMPLE_COUNT) {
		virq_cycles[sequence] = k_cycle_get_64();
		virq_count = sequence + 1;
	}
}

int main(void)
{
	IRQ_CONNECT(SOFTWARE_VIRQ, 1, software_virq_isr, NULL, 0);
	irq_enable(SOFTWARE_VIRQ);

	printk("SOFTWARE VIRQ READY vector=%d samples=%d\n", SOFTWARE_VIRQ,
	       SAMPLE_COUNT);

	const int64_t start_ticks = k_uptime_ticks();
	const int64_t timeout_ticks = k_ms_to_ticks_ceil64(WAIT_TIMEOUT_MS);
	while (virq_count < SAMPLE_COUNT &&
	       k_uptime_ticks() - start_ticks < timeout_ticks) {
		k_msleep(1);
	}

	const uint32_t received = virq_count;
	printk("sequence,timestamp_ns\n");
	for (uint32_t sequence = 0; sequence < received; sequence++) {
		printk("%u,%llu\n", sequence,
		       k_cyc_to_ns_floor64(virq_cycles[sequence]));
	}

	if (received == SAMPLE_COUNT) {
		printk("SOFTWARE VIRQ COMPLETE samples=%d\n", SAMPLE_COUNT);
	} else {
		printk("SOFTWARE VIRQ FAIL received=%u expected=%d\n", received,
		       SAMPLE_COUNT);
	}
	return received == SAMPLE_COUNT ? 0 : 1;
}
