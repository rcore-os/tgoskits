#include <stdint.h>

#include <zephyr/irq.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/time_units.h>

#define SOFTWARE_VIRQ_A 48
#define SOFTWARE_VIRQ_B 49
#define SAMPLE_COUNT 300
#define WAIT_TIMEOUT_MS 30000

static volatile uint32_t virq_count[2];
static volatile uint64_t virq_cycles[2][SAMPLE_COUNT];
static const uint8_t stream_a;
static const uint8_t stream_b = 1;

static void software_virq_isr(const void *arg)
{
	const uint8_t stream = *(const uint8_t *)arg;

	const uint32_t sequence = virq_count[stream];
	if (sequence < SAMPLE_COUNT) {
		virq_cycles[stream][sequence] = k_cycle_get_64();
		virq_count[stream] = sequence + 1;
	}
}

int main(void)
{
	IRQ_CONNECT(SOFTWARE_VIRQ_A, 1, software_virq_isr, &stream_a, 0);
	IRQ_CONNECT(SOFTWARE_VIRQ_B, 1, software_virq_isr, &stream_b, 0);
	irq_enable(SOFTWARE_VIRQ_A);
	irq_enable(SOFTWARE_VIRQ_B);

	printk("SOFTWARE VIRQ READY vectors=%d,%d samples=%d streams=2\n",
	       SOFTWARE_VIRQ_A, SOFTWARE_VIRQ_B, SAMPLE_COUNT);

	const int64_t start_ticks = k_uptime_ticks();
	const int64_t timeout_ticks = k_ms_to_ticks_ceil64(WAIT_TIMEOUT_MS);
	while ((virq_count[0] < SAMPLE_COUNT || virq_count[1] < SAMPLE_COUNT) &&
	       k_uptime_ticks() - start_ticks < timeout_ticks) {
		k_msleep(1);
	}

	const uint32_t received_a = virq_count[0];
	const uint32_t received_b = virq_count[1];
	printk("vector,sequence,timestamp_ns\n");
	for (uint32_t sequence = 0; sequence < received_a; sequence++) {
		printk("%d,%u,%llu\n", SOFTWARE_VIRQ_A, sequence,
		       k_cyc_to_ns_floor64(virq_cycles[0][sequence]));
	}
	for (uint32_t sequence = 0; sequence < received_b; sequence++) {
		printk("%d,%u,%llu\n", SOFTWARE_VIRQ_B, sequence,
		       k_cyc_to_ns_floor64(virq_cycles[1][sequence]));
	}

	const bool complete = received_a == SAMPLE_COUNT && received_b == SAMPLE_COUNT;
	if (complete) {
		printk("SOFTWARE VIRQ COMPLETE streams=2 samples_each=%d total=%d\n",
		       SAMPLE_COUNT, 2 * SAMPLE_COUNT);
	} else {
		printk("SOFTWARE VIRQ FAIL received=%u,%u expected_each=%d\n",
		       received_a, received_b, SAMPLE_COUNT);
	}
	return complete ? 0 : 1;
}
