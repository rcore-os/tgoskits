#define _GNU_SOURCE
#include "ivc_sdk.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define PCI_DEVICES_DIR "/sys/bus/pci/devices"
#define IVSHMEM_VENDOR "0x1af4"
#define IVSHMEM_DEVICE "0x1110"
#define IVSHMEM_BAR0_DOORBELL_WORD 3U
#define IVSHMEM_BAR0_SIZE 0x1000U
#define IVSHMEM_BAR2_INDEX 2U
#define IVSHMEM_BAR2_SIZE 0x200000U
#define IVC_PLATFORM_RING_SIZE 65536U

struct ivc_default_platform {
    struct ivc_sdk *sdk;
    struct ivc_pending_entry pending[8];
    void *shared_mem;
    size_t shared_mem_size;
    volatile uint32_t *doorbell_regs;
    uint32_t doorbell_value;
};

static struct ivc_default_platform g_platform;

static int read_trimmed(const char *path, char *buf, size_t len)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }

    ssize_t n = read(fd, buf, len - 1);
    close(fd);
    if (n <= 0) {
        return -1;
    }
    while (n > 0 && (buf[n - 1] == '\n' || buf[n - 1] == '\r')) {
        n--;
    }
    buf[n] = '\0';
    return 0;
}

static int find_ivshmem(char *path, size_t len)
{
    DIR *dir = opendir(PCI_DEVICES_DIR);
    if (!dir) {
        return -1;
    }

    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        if (entry->d_name[0] == '.') {
            continue;
        }

        char dev[512], vendor_path[640], device_path[640], vendor[32],
            device[32];
        snprintf(dev, sizeof(dev), "%s/%s", PCI_DEVICES_DIR, entry->d_name);
        snprintf(vendor_path, sizeof(vendor_path), "%s/vendor", dev);
        snprintf(device_path, sizeof(device_path), "%s/device", dev);

        if (read_trimmed(vendor_path, vendor, sizeof(vendor)) == 0 &&
            read_trimmed(device_path, device, sizeof(device)) == 0 &&
            strcmp(vendor, IVSHMEM_VENDOR) == 0 &&
            strcmp(device, IVSHMEM_DEVICE) == 0) {
            snprintf(path, len, "%s", dev);
            closedir(dir);
            printf("ivshmem-pci vendor=%s device=%s bdf=%s\n", vendor, device,
                   entry->d_name);
            return 0;
        }
    }

    closedir(dir);
    errno = ENODEV;
    return -1;
}

static int resource_size(const char *dev, unsigned index, size_t *size)
{
    char path[768];
    snprintf(path, sizeof(path), "%s/resource", dev);
    FILE *fp = fopen(path, "r");
    if (!fp) {
        return -1;
    }

    for (unsigned i = 0; i <= index; i++) {
        unsigned long long start = 0, end = 0, flags = 0;
        if (fscanf(fp, "%llx %llx %llx", &start, &end, &flags) != 3) {
            fclose(fp);
            errno = EINVAL;
            return -1;
        }
        if (i == index) {
            fclose(fp);
            if (end < start) {
                errno = EINVAL;
                return -1;
            }
            *size = (size_t)(end - start + 1);
            printf("ivshmem resource%u base=0x%llx size=%zu flags=0x%llx\n",
                   index, start, *size, flags);
            return 0;
        }
    }

    fclose(fp);
    errno = ENODEV;
    return -1;
}

static void *map_pci_resource(const char *dev, unsigned resource_index,
                              size_t expected_size)
{
    size_t size = 0;
    char path[768];

    if (resource_size(dev, resource_index, &size) != 0) {
        return MAP_FAILED;
    }
    if (size < expected_size) {
        errno = EINVAL;
        return MAP_FAILED;
    }

    snprintf(path, sizeof(path), "%s/resource%u", dev, resource_index);
    int fd = open(path, O_RDWR | O_SYNC);
    if (fd < 0) {
        return MAP_FAILED;
    }

    void *addr = mmap(NULL, expected_size, PROT_READ | PROT_WRITE, MAP_SHARED,
                      fd, 0);
    close(fd);
    if (addr != MAP_FAILED) {
        printf("ivshmem map resource%u\n", resource_index);
    }
    return addr;
}

static void platform_doorbell(void *ctx)
{
    struct ivc_default_platform *platform = ctx;
    platform->doorbell_regs[IVSHMEM_BAR0_DOORBELL_WORD] =
        platform->doorbell_value;
}

int ivc_sdk_open_default(struct ivc_sdk *sdk, enum ivc_peer peer)
{
    char dev[512];
    struct ivc_default_platform *platform = &g_platform;

    if (sdk == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    if (peer != IVC_PEER_LINUX) {
        return IVC_ERR_INVALID_ARG;
    }

    ivc_sdk_close(sdk);
    if (find_ivshmem(dev, sizeof(dev)) != 0) {
        return IVC_ERR_NOT_FOUND;
    }

    platform->doorbell_regs = map_pci_resource(dev, 0, IVSHMEM_BAR0_SIZE);
    if (platform->doorbell_regs == MAP_FAILED) {
        platform->doorbell_regs = NULL;
        return IVC_ERR_INVALID_ARG;
    }

    platform->shared_mem = map_pci_resource(dev, IVSHMEM_BAR2_INDEX,
                                             IVSHMEM_BAR2_SIZE);
    if (platform->shared_mem == MAP_FAILED) {
        platform->shared_mem = NULL;
        ivc_sdk_close(sdk);
        return IVC_ERR_INVALID_ARG;
    }
    platform->shared_mem_size = IVSHMEM_BAR2_SIZE;

    if (ivc_sdk_shared_init(platform->shared_mem, platform->shared_mem_size,
                            IVC_PLATFORM_RING_SIZE,
                            IVC_PLATFORM_RING_SIZE) != IVC_OK) {
        errno = EINVAL;
        ivc_sdk_close(sdk);
        return IVC_ERR_INVALID_ARG;
    }

    platform->doorbell_value = (1U << 16) | 1U;
    if (ivc_sdk_init(sdk, platform->shared_mem,
                     platform->shared_mem_size, IVC_PEER_LINUX,
                     platform_doorbell, platform, NULL, NULL) != IVC_OK) {
        errno = EINVAL;
        ivc_sdk_close(sdk);
        return IVC_ERR_INVALID_ARG;
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
    if (platform->shared_mem != NULL) {
        munmap(platform->shared_mem, platform->shared_mem_size);
    }
    if (platform->doorbell_regs != NULL) {
        munmap((void *)platform->doorbell_regs, IVSHMEM_BAR0_SIZE);
    }
    if (platform->sdk != NULL) {
        memset(platform->sdk, 0, sizeof(*platform->sdk));
    } else if (sdk != NULL) {
        memset(sdk, 0, sizeof(*sdk));
    }
    memset(platform, 0, sizeof(*platform));
}
