#include <stdint.h>

#include <zephyr/arch/arm64/arm-smccc.h>
#include <zephyr/irq.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/time_units.h>

#define SOFTWARE_VIRQ 48
#define SAMPLE_COUNT 300
#define WAIT_TIMEOUT_MS 30000

/*
 * The vCPU idles by executing PSCI CPU_SUSPEND(standby) through HVC. The
 * hypervisor parks the vCPU on its wait queue, which makes the notify path
 * load-bearing: A's notify_all also wakes the idle vCPU, while B's targeted
 * notify only wakes the IRQ consumer.
 */
#define PSCI_CPU_SUSPEND_64 0xC4000001UL
#define PSCI_POWER_STATE_STANDBY 0UL
#define PSCI_SYSTEM_OFF_32 0x84000008UL

static volatile uint32_t virq_count;
static volatile uint64_t virq_cycles[SAMPLE_COUNT];

static void suspend_cpu(void)
{
	struct arm_smccc_res res;

	arm_smccc_hvc(PSCI_CPU_SUSPEND_64, PSCI_POWER_STATE_STANDBY, 0,
		      0, 0, 0, 0, 0, &res);
}

static void psci_system_off(void)
{
	struct arm_smccc_res res;

	arm_smccc_hvc(PSCI_SYSTEM_OFF_32, 0, 0, 0, 0, 0, 0, 0, &res);
}

static void software_virq_isr(const void *arg)
{
	ARG_UNUSED(arg);

	const uint32_t sequence = virq_count;
	if (sequence < SAMPLE_COUNT) {
		virq_cycles[sequence] = k_cycle_get_64();
		virq_count = sequence + 1;
		if (sequence % 100 == 0) {
			printk("ISR seq=%u on cpu %d\n", sequence,
			       arch_curr_cpu()->id);
		}
	}
}

static K_THREAD_STACK_DEFINE(consumer_stack, 4096);
static struct k_thread consumer_thread;

static void consumer_entry(void *arg1, void *arg2, void *arg3)
{
	ARG_UNUSED(arg1);
	ARG_UNUSED(arg2);
	ARG_UNUSED(arg3);

	printk("consumer thread on cpu %d\n", arch_curr_cpu()->id);
	const int64_t start_ticks = k_uptime_ticks();
	uint32_t loop_count = 0;
	while (virq_count < SAMPLE_COUNT &&
	       k_uptime_ticks() - start_ticks <
		       k_ms_to_ticks_ceil64(WAIT_TIMEOUT_MS)) {
		suspend_cpu();
		if (++loop_count % 50 == 0) {
			printk("consumer loop=%u irq=%u\n", loop_count, virq_count);
		}
	}

	const uint32_t received = virq_count;
	printk("consumer done irq=%u\n", received);
	/* Let the host injector finish its last log lines before the CSV hits
	 * the same serial line, so the two streams do not interleave mid-line.
	 */
	const uint64_t dump_start = k_cycle_get_64();
	while (k_cyc_to_ns_floor64(k_cycle_get_64() - dump_start) < 10000000ULL) {
	}
	printk("vector,sequence,timestamp_ns\n");
	for (uint32_t sequence = 0; sequence < received; sequence++) {
		printk("%d,%u,%llu\n", SOFTWARE_VIRQ, sequence,
		       k_cyc_to_ns_floor64(virq_cycles[sequence]));
	}

	if (received == SAMPLE_COUNT) {
		printk("SOFTWARE VIRQ COMPLETE streams=1 samples_each=%d total=%d\n",
		       SAMPLE_COUNT, SAMPLE_COUNT);
	} else {
		printk("SOFTWARE VIRQ FAIL received=%u expected=%d\n", received,
		       SAMPLE_COUNT);
	}
	psci_system_off();
}

int main(void)
{
	IRQ_CONNECT(SOFTWARE_VIRQ, 1, software_virq_isr, NULL, 0);
	irq_enable(SOFTWARE_VIRQ);

	printk("SOFTWARE VIRQ READY suspend streams=1 vector=%d samples=%d\n",
	       SOFTWARE_VIRQ, SAMPLE_COUNT);

	k_thread_create(&consumer_thread, consumer_stack,
			K_THREAD_STACK_SIZEOF(consumer_stack),
			consumer_entry, NULL, NULL, NULL,
			K_PRIO_PREEMPT(0), 0, K_FOREVER);
	const int pin_rc = k_thread_cpu_pin(&consumer_thread, 1);
	printk("consumer pinned to cpu 1 rc=%d\n", pin_rc);
	k_thread_start(&consumer_thread);

	for (;;) {
		suspend_cpu();
	}
	return 0;
}
