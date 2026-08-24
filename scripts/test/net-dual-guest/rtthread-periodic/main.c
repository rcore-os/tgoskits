/*
 * RT-Thread 10 ms periodic wake-up latency sampler for the Task 1 A/B.
 *
 * Mirrors the Zephyr probe contract (scripts/test/zephyr-periodic):
 * - waits for a single 'g' byte on the console before sampling,
 * - records a build-configurable number of wake-ups at a configurable period,
 * - waits for a 'd' byte after sampling so CSV export is outside measurement,
 * - prints CSV rows (sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns),
 * - prints "PERIODIC LATENCY COMPLETE samples=300" when done.
 *
 * Time and wake-ups use the AArch64 virtual timer (CNTVCT_EL0 /
 * CNTV_CVAL_EL0). AxVisor passes the virtual timer through in hardware, so
 * the measured jitter is not quantized by RT-Thread's software tick (which
 * is backed by the emulated physical timer and only advances at the host
 * timer-wheel granularity). The periodic build exposes the virtual timer
 * PPI (INTID 27) in the GIC driver and returns the IRQ slot the GIC handler
 * actually dispatches through.
 */

#include <stdint.h>
#include <string.h>

#include <ioremap.h>
#include <rtdevice.h>
#include <rthw.h>
#include <rtthread.h>

#ifndef PERIOD_MS
#define PERIOD_MS 10
#endif

#ifndef SAMPLE_COUNT
#define SAMPLE_COUNT 300
#endif
#define UART0_PHYS UINT64_C(0x09000000)
#define PL011_DR 0x000
#define PL011_FR 0x018
#define PL011_FR_RXFE (1u << 4)

extern rt_ubase_t rt_pic_arch_timer_virtual_irq(void);

struct latency_sample {
	int64_t timestamp_ns;
	int64_t deadline_ns;
	int64_t actual_ns;
	int64_t jitter_ns;
};

static struct latency_sample samples[SAMPLE_COUNT];

static uint64_t read_cntvct(void)
{
	uint64_t cycles;

	__asm__ volatile("mrs %0, cntvct_el0" : "=r"(cycles));
	return cycles;
}

static uint64_t read_cntfrq(void)
{
	uint64_t freq;

	__asm__ volatile("mrs %0, cntfrq_el0" : "=r"(freq));
	return freq;
}

static int64_t cycles_to_ns(uint64_t cycles, uint64_t freq)
{
	uint64_t seconds;
	uint64_t remainder;

	if (freq == 0) {
		return 0;
	}
	/*
	 * Split before scaling. At 24 MHz, cycles * 1e9 overflows uint64_t
	 * after about 768 seconds even though the converted duration fits.
	 */
	seconds = cycles / freq;
	remainder = cycles % freq;
	return (int64_t)(seconds * UINT64_C(1000000000) +
			 remainder * UINT64_C(1000000000) / freq);
}

static int64_t signed_cycles_to_ns(int64_t cycles, uint64_t freq)
{
	int64_t signed_freq;
	int64_t seconds;
	int64_t remainder;

	if (freq == 0) {
		return 0;
	}
	signed_freq = (int64_t)freq;
	seconds = cycles / signed_freq;
	remainder = cycles % signed_freq;
	return seconds * INT64_C(1000000000) +
	       remainder * INT64_C(1000000000) / signed_freq;
}

static volatile int vtimer_fired;

static void vtimer_isr(int vector, void *parameter)
{
	(void)vector;
	(void)parameter;
	/* Disable the virtual timer; the level IRQ deasserts with it. */
	__asm__ volatile("msr cntv_ctl_el0, xzr");
	vtimer_fired = 1;
}

static void vtimer_init(void)
{
	rt_ubase_t irq = rt_pic_arch_timer_virtual_irq();

	rt_hw_interrupt_install((int)irq, vtimer_isr, RT_NULL, "periodic-vtimer");
	rt_hw_interrupt_umask((int)irq);
}

static void sleep_until_cycles(uint64_t deadline_cycles)
{
	vtimer_fired = 0;
	__asm__ volatile("msr cntv_cval_el0, %0" ::"r"(deadline_cycles));
	__asm__ volatile("msr cntv_ctl_el0, %0" ::"r"((uint64_t)1));
	__asm__ volatile("isb");
	while (!vtimer_fired) {
		__asm__ volatile("wfi");
	}
}

static volatile uint32_t *uart_base;
static rt_device_t console_device;

static int uart_init(void)
{
	uart_base = (volatile uint32_t *)rt_ioremap((void *)UART0_PHYS, 0x1000);
	if (uart_base == RT_NULL) {
		return -1;
	}
	return 0;
}

static int uart_getc(void)
{
	if (uart_base == RT_NULL) {
		return -1;
	}
	/* FR.RXFE is set when the receive FIFO is empty. */
	if (uart_base[PL011_FR / 4] & PL011_FR_RXFE) {
		return -1;
	}
	return (int)(uart_base[PL011_DR / 4] & 0xff);
}

static int control_getc(void)
{
	char byte = 0;
	int ch = uart_getc();

	if (console_device != RT_NULL &&
	    rt_device_read(console_device, 0, &byte, 1) == 1) {
		return (int)byte;
	}
	return ch;
}

static int wait_for_start(void)
{
	int ch = -1;

	if (uart_init() != 0) {
		rt_kprintf("PERIODIC LATENCY ERROR uart-map-failed\n");
		return -1;
	}
	console_device = rt_device_find(RT_CONSOLE_DEVICE_NAME);
	if (console_device != RT_NULL &&
	    rt_device_open(console_device,
			   RT_DEVICE_OFLAG_RDWR | RT_DEVICE_FLAG_INT_RX) != RT_EOK) {
		rt_kprintf("PERIODIC LATENCY ERROR console-open-failed\n");
		console_device = RT_NULL;
	}

	rt_kprintf("PERIODIC LATENCY READY\n");
	while (ch != 'g') {
		ch = control_getc();
		if (ch != 'g') {
			rt_thread_mdelay(1);
		}
	}
	rt_kprintf("PERIODIC LATENCY START\n");
	return 0;
}

static void wait_for_dump(void)
{
	int ch = -1;

	while (ch != 'd') {
		ch = control_getc();
		if (ch != 'd') {
			rt_thread_mdelay(1);
		}
	}
}

int main(void)
{
	uint64_t freq = read_cntfrq();
	uint64_t period_cycles = freq * PERIOD_MS / 1000;
	rt_tick_t base_ticks;
	uint64_t base_cycles;
	uint64_t next_deadline_cycles;
	int64_t sequence;

	if (freq == 0 || period_cycles == 0) {
		rt_kprintf("PERIODIC LATENCY ERROR counter-frequency=%llu\n",
			   (unsigned long long)freq);
		return 1;
	}

	if (wait_for_start() != 0) {
		return 1;
	}
	vtimer_init();

	base_ticks = rt_tick_get();
	while (rt_tick_get() == base_ticks) {
	}
	base_ticks = rt_tick_get();
	base_cycles = read_cntvct();
	/*
	 * Anchor each deadline to the previous actual wake-up plus the period
	 * (relative-period semantics). This measures the per-wake scheduling
	 * delay, matching the Zephyr probe's jitter definition. Absolute
	 * deadlines instead accumulate lateness when the vCPU is serviced less
	 * often than the period, which inflated the earlier RR baseline.
	 */
	next_deadline_cycles = base_cycles + period_cycles;

	for (sequence = 0; sequence < SAMPLE_COUNT; sequence++) {
		uint64_t deadline_cycles;
		uint64_t actual_cycles;

		sleep_until_cycles(next_deadline_cycles);
		actual_cycles = read_cntvct();
		deadline_cycles = next_deadline_cycles;
		samples[sequence].timestamp_ns =
			cycles_to_ns(actual_cycles - base_cycles, freq);
		samples[sequence].deadline_ns =
			cycles_to_ns(deadline_cycles - base_cycles, freq);
		samples[sequence].actual_ns =
			cycles_to_ns(actual_cycles - base_cycles, freq);
		samples[sequence].jitter_ns =
			signed_cycles_to_ns(
					(int64_t)actual_cycles - (int64_t)deadline_cycles,
					freq);
		next_deadline_cycles = actual_cycles + period_cycles;
	}

	rt_kprintf("PERIODIC LATENCY SAMPLING COMPLETE samples=%d\n",
		   SAMPLE_COUNT);
	wait_for_dump();
	rt_kprintf("sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns\n");
	for (sequence = 0; sequence < SAMPLE_COUNT; sequence++) {
		rt_kprintf("%ld,%ld,%ld,%ld,%ld\n",
			   (long)sequence,
			   (long)samples[sequence].timestamp_ns,
			   (long)samples[sequence].deadline_ns,
			   (long)samples[sequence].actual_ns,
			   (long)samples[sequence].jitter_ns);
	}
	rt_kprintf("PERIODIC LATENCY COMPLETE samples=%d\n", SAMPLE_COUNT);
	return 0;
}
