#include <stdint.h>
#include <stddef.h>

#include <zephyr/arch/arm64/arm_mem.h>
#include <zephyr/kernel.h>
#include <zephyr/kernel/mm.h>
#include <zephyr/sys/device_mmio.h>
#include <zephyr/sys/printk.h>

#define IVSHMEM_BAR0_PHYS 0x50100000UL
#define IVSHMEM_BAR0_SIZE 0x1000UL
#define IVSHMEM_BAR2_PHYS 0x50200000UL
#define IVSHMEM_BAR2_SIZE 0x200000UL

#define PAYLOAD_SIZE 4096U
#define MAGIC 0x41584232U
#define A_TO_B_SEQ 1U
#define B_TO_A_SEQ 2U
#define BAR0_DOORBELL_WORD 3U
#define TIMEOUT_MS 60000
#define POLL_INTERVAL_US 1000
#define PSCI_SYSTEM_OFF 0x84000008UL

struct bar2_mailbox {
	volatile uint32_t magic;
	volatile uint32_t a_seq;
	volatile uint32_t b_seq;
	volatile uint32_t a_checksum;
	volatile uint32_t b_checksum;
	volatile uint8_t a_payload[PAYLOAD_SIZE];
	volatile uint8_t b_payload[PAYLOAD_SIZE];
};

static uint32_t checksum(const volatile uint8_t *data, size_t len)
{
	uint32_t sum = 0x12345678U;

	for (size_t i = 0; i < len; i++) {
		sum = (sum << 5) | (sum >> 27);
		sum ^= data[i];
		sum += (uint32_t)i;
	}
	return sum;
}

static void fill_payload(volatile uint8_t *data, size_t len, uint32_t seed)
{
	uint32_t x = seed;

	for (size_t i = 0; i < len; i++) {
		x = x * 1664525U + 1013904223U;
		data[i] = (uint8_t)(x >> 24);
	}
}

static void write_doorbell(volatile uint32_t *bar0, uint32_t target_peer, uint32_t vector)
{
	bar0[BAR0_DOORBELL_WORD] = (target_peer << 16) | (vector & 0xffffU);
	__sync_synchronize();
}

static void psci_system_off(void)
{
	register uint64_t x0 __asm__("x0") = PSCI_SYSTEM_OFF;

	__asm__ volatile("smc #0" : "+r"(x0) : : "memory");
}

static int wait_for(volatile uint32_t *field, uint32_t expected, const char *what)
{
	for (uint32_t i = 0; i < TIMEOUT_MS; i++) {
		if (*field == expected) {
			__sync_synchronize();
			return 0;
		}
		k_busy_wait(POLL_INTERVAL_US);
	}
	printk("Zephyr ivshmem failed: timeout waiting for %s value=0x%x expected=0x%x\n",
	       what, *field, expected);
	return -1;
}

int main(void)
{
	mm_reg_t bar0_addr = 0;
	mm_reg_t bar2_addr = 0;

	printk("Zephyr ivshmem peer init\n");
	device_map(&bar0_addr, IVSHMEM_BAR0_PHYS, IVSHMEM_BAR0_SIZE, K_MEM_ARM_DEVICE_nGnRE);
	device_map(&bar2_addr, IVSHMEM_BAR2_PHYS, IVSHMEM_BAR2_SIZE, K_MEM_ARM_DEVICE_nGnRE);

	volatile uint32_t *bar0 = (volatile uint32_t *)bar0_addr;
	struct bar2_mailbox *box = (struct bar2_mailbox *)bar2_addr;

	printk("Zephyr ivshmem mapped bar0=0x%lx bar2=0x%lx\n",
	       (unsigned long)bar0_addr, (unsigned long)bar2_addr);
	printk("Zephyr ivshmem peer ready\n");

	for (uint32_t i = 0; i < TIMEOUT_MS; i++) {
		if (box->magic == MAGIC) {
			break;
		}
		k_busy_wait(POLL_INTERVAL_US);
	}
	if (box->magic != MAGIC) {
		printk("Zephyr ivshmem failed: timeout waiting for BAR2 magic "
		       "magic=0x%x a_seq=0x%x b_seq=0x%x a_checksum=0x%x\n",
		       box->magic, box->a_seq, box->b_seq, box->a_checksum);
		return 1;
	}
	printk("Zephyr sees BAR2 magic a_seq=0x%x a_checksum=0x%x\n",
	       box->a_seq, box->a_checksum);

	if (wait_for(&box->a_seq, A_TO_B_SEQ, "Linux payload") != 0) {
		return 1;
	}
	printk("Zephyr reads Linux BAR2 payload\n");
	if (checksum(box->a_payload, PAYLOAD_SIZE) != box->a_checksum) {
		printk("Zephyr ivshmem failed: Linux payload checksum mismatch\n");
		return 1;
	}

	fill_payload(box->b_payload, PAYLOAD_SIZE, 0xb22d0001U);
	box->b_checksum = checksum(box->b_payload, PAYLOAD_SIZE);
	__sync_synchronize();
	printk("Zephyr writes BAR2 response\n");
	box->b_seq = B_TO_A_SEQ;
	__sync_synchronize();
	write_doorbell(bar0, 0, 2);
	printk("Zephyr writes doorbell(target=Linux)\n");
	printk("Zephyr ivshmem shared memory pass\n");

	k_sleep(K_MSEC(100));
	psci_system_off();
	printk("Zephyr ivshmem failed: PSCI SYSTEM_OFF returned\n");
	return 1;
}
