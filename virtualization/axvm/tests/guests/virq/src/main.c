#include <stdint.h>
#include <zephyr/arch/arm64/arm-smccc.h>
#include <zephyr/irq.h>
#include <zephyr/kernel.h>
#include <zephyr/dt-bindings/interrupt-controller/arm-gic.h>

#define SOFTWARE_VIRQ 48
#define SAMPLE_COUNT 300
#define TIMER_IRQ 27
#define PSCI_CPU_SUSPEND_64 0xc4000001UL
#define PSCI_SYSTEM_OFF_32 0x84000008UL

/* Test-only mailbox in normal coherent guest RAM. Each word has one writer:
 * CPU0 owns ready0/parks0, CPU1 owns ready1/received, host owns complete.
 * The build tool obtains its GPA from the ELF symbol, never a guessed address.
 */
struct virq_mailbox {
	uint32_t ready0;
	uint32_t ready1;
	uint32_t received;
	uint32_t parks0;
	uint32_t complete;
};
struct virq_mailbox virq_mailbox;

static void suspend_cpu(void)
{
	struct arm_smccc_res res;
	arm_smccc_hvc(PSCI_CPU_SUSPEND_64, 0, 0, 0, 0, 0, 0, 0, &res);
	if (res.a0 != 0) {
		printk("SOFTWARE VIRQ FAIL suspend=%ld\n", res.a0);
		k_panic();
	}
}

static void software_virq_isr(const void *arg)
{
	ARG_UNUSED(arg);
	uint32_t count = __atomic_load_n(&virq_mailbox.received, __ATOMIC_RELAXED);
	if (arch_curr_cpu()->id != 1 || count >= SAMPLE_COUNT) {
		printk("SOFTWARE VIRQ FAIL cpu=%d count=%u\n", arch_curr_cpu()->id, count);
		k_panic();
	}
	__atomic_store_n(&virq_mailbox.received, count + 1, __ATOMIC_RELEASE);
}

static K_THREAD_STACK_DEFINE(consumer_stack, 4096);
static struct k_thread consumer_thread;

static void consumer_entry(void *a, void *b, void *c)
{
	ARG_UNUSED(a);
	ARG_UNUSED(b);
	ARG_UNUSED(c);
	/* Enabling the SPI on CPU1 sets its GICv3 routing affinity to CPU1. */
	irq_enable(SOFTWARE_VIRQ);
	irq_disable(TIMER_IRQ);
	__atomic_store_n(&virq_mailbox.ready1, 1, __ATOMIC_RELEASE);
	while (__atomic_load_n(&virq_mailbox.received, __ATOMIC_ACQUIRE) < SAMPLE_COUNT) {
		suspend_cpu();
	}
	/* The host acknowledges that it checked all deliveries and idle-CPU
	 * isolation before we power off. No additional interrupt is injected.
	 */
	while (!__atomic_load_n(&virq_mailbox.complete, __ATOMIC_ACQUIRE)) {
		arch_nop();
	}
	printk("SOFTWARE VIRQ COMPLETE streams=1 samples_each=300 total=300\n");
	struct arm_smccc_res res;
	arm_smccc_hvc(PSCI_SYSTEM_OFF_32, 0, 0, 0, 0, 0, 0, 0, &res);
	k_panic();
}

int main(void)
{
	IRQ_CONNECT(SOFTWARE_VIRQ, 1, software_virq_isr, NULL, IRQ_TYPE_EDGE);
	k_thread_create(&consumer_thread, consumer_stack, K_THREAD_STACK_SIZEOF(consumer_stack),
			consumer_entry, NULL, NULL, NULL, K_PRIO_PREEMPT(0), 0, K_FOREVER);
	if (k_thread_cpu_pin(&consumer_thread, 1) != 0) {
		printk("SOFTWARE VIRQ FAIL pin\n");
		return 1;
	}
	k_thread_start(&consumer_thread);
	irq_disable(TIMER_IRQ);
	printk("SOFTWARE VIRQ READY suspend vector=48 samples=300\n");
	__atomic_store_n(&virq_mailbox.ready0, 1, __ATOMIC_RELEASE);
	for (;;) {
		__atomic_fetch_add(&virq_mailbox.parks0, 1, __ATOMIC_RELEASE);
		suspend_cpu();
	}
	return 0;
}
