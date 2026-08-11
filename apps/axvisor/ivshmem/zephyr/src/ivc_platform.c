#include "ivc_sdk.h"

#include <stdint.h>
#include <string.h>
#include <zephyr/arch/arm64/arm_mem.h>
#include <zephyr/kernel.h>
#include <zephyr/kernel/mm.h>
#include <zephyr/sys/printk.h>

#define IVSHMEM_BAR0_PHYS 0x50100000UL
#define IVSHMEM_BAR0_SIZE 0x1000UL
#define IVSHMEM_BAR2_PHYS 0x50200000UL
#define IVSHMEM_BAR2_SIZE 0x200000UL
#define IVSHMEM_BAR0_DOORBELL_WORD 3U
#define TIMEOUT_LOOPS 60000U
#define POLL_INTERVAL_US 1000U

struct ivc_default_platform {
    struct ivc_sdk *sdk;
    struct ivc_pending_entry pending[8];
    void *shared_mem;
    uint32_t shared_mem_size;
    volatile uint32_t *doorbell_regs;
    uint32_t doorbell_value;
};

static struct ivc_default_platform g_platform;

static void platform_doorbell(void *ctx)
{
    struct ivc_default_platform *platform = ctx;
    platform->doorbell_regs[IVSHMEM_BAR0_DOORBELL_WORD] =
        platform->doorbell_value;
}

static int wait_shared_ready(void *bar2)
{
    volatile struct ivc_shared_header *shared =
        (volatile struct ivc_shared_header *)bar2;

    for (uint32_t i = 0; i < TIMEOUT_LOOPS; i++) {
        if (shared->magic == IVC_SHARED_MAGIC &&
            shared->version == IVC_SHARED_VERSION &&
            shared->header_len == IVC_SHARED_HEADER_SIZE) {
            __sync_synchronize();
            return 0;
        }
        k_busy_wait(POLL_INTERVAL_US);
    }
    printk("Zephyr ivshmem failed: timeout waiting for shared header\n");
    return -1;
}

int ivc_sdk_open_default(struct ivc_sdk *sdk, enum ivc_peer peer)
{
    struct ivc_default_platform *platform = &g_platform;
    mm_reg_t bar0_addr = 0;
    mm_reg_t bar2_addr = 0;
    int rc;

    if (sdk == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    if (peer != IVC_PEER_ZEPHYR) {
        return IVC_ERR_INVALID_ARG;
    }

    ivc_sdk_close(sdk);
    memset(platform, 0, sizeof(*platform));
    device_map(&bar0_addr, IVSHMEM_BAR0_PHYS, IVSHMEM_BAR0_SIZE,
               K_MEM_ARM_DEVICE_nGnRE);
    device_map(&bar2_addr, IVSHMEM_BAR2_PHYS, IVSHMEM_BAR2_SIZE,
               K_MEM_ARM_NORMAL_NC);
    printk("Zephyr ivshmem mapped bar0=0x%lx bar2=0x%lx\n",
           (unsigned long)bar0_addr, (unsigned long)bar2_addr);

    if (wait_shared_ready((void *)bar2_addr) != 0) {
        return IVC_ERR_TIMEOUT;
    }

    platform->doorbell_regs = (volatile uint32_t *)bar0_addr;
    platform->doorbell_value = 2U;
    platform->shared_mem = (void *)bar2_addr;
    platform->shared_mem_size = IVSHMEM_BAR2_SIZE;

    rc = ivc_sdk_init(sdk, platform->shared_mem,
                      platform->shared_mem_size, IVC_PEER_ZEPHYR,
                      platform_doorbell, platform, NULL, NULL);
    if (rc != IVC_OK) {
        printk("Zephyr ivshmem failed: SDK init failed rc=%d\n", rc);
        ivc_sdk_close(sdk);
        return rc;
    }

    platform->sdk = sdk;
    ivc_sdk_set_pending_table(sdk, platform->pending,
                              sizeof(platform->pending) /
                                  sizeof(platform->pending[0]));
    return IVC_OK;
}

void ivc_sdk_close(struct ivc_sdk *sdk)
{
    struct ivc_default_platform *platform = &g_platform;

    if (sdk != NULL && platform->sdk != NULL && platform->sdk != sdk) {
        return;
    }
    if (platform->sdk != NULL) {
        memset(platform->sdk, 0, sizeof(*platform->sdk));
    } else if (sdk != NULL) {
        memset(sdk, 0, sizeof(*sdk));
    }
    memset(platform, 0, sizeof(*platform));
}
