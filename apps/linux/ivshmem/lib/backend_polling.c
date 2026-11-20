/*
 * BAR mapping, register access, and the polling event backend.
 *
 * All addressing is BAR-relative: the adapter reads each BAR's size from the
 * sysfs `resource` table and maps the corresponding `resourceN` file, so no
 * caller ever stores a hypervisor-assigned absolute address. The polling
 * backend owns the W1C handshake on Event Status so callers cannot get the
 * clear-then-recheck order wrong.
 */
#define _XOPEN_SOURCE 700

#include "ivshmem_internal.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

/* Milliseconds between Event Status polls; short enough for smoke timeouts,
 * slow enough not to saturate a single guest CPU. */
#define IVSHMEM_POLL_INTERVAL_MS 1

#define IVSHMEM_UIO_CLASS "/sys/class/uio"
#define IVSHMEM_DEVICE_ROOT "/dev"

struct ivshmem_backend {
    struct ivshmem_device *dev;
    enum ivshmem_backend_kind kind;
    int interrupt_fd;
};

int ivshmem_enable_device(struct ivshmem_device *dev)
{
    char path[IVSHMEM_PATH_MAX];
    uint16_t command = 0;
    ssize_t transferred;
    int fd;

    if (dev == NULL) {
        return IVSHMEM_ERR_ARGS;
    }
    snprintf(path, sizeof(path), "%s/config", dev->sysfs_dir);
    fd = open(path, O_RDWR);
    if (fd < 0) {
        return IVSHMEM_ERR_IO;
    }
    transferred = pread(fd, &command, sizeof(command),
                        IVSHMEM_CONFIG_COMMAND_OFFSET);
    if (transferred != (ssize_t)sizeof(command)) {
        close(fd);
        return IVSHMEM_ERR_IO;
    }
    command |= IVSHMEM_COMMAND_MEMORY_ENABLE;
    transferred = pwrite(fd, &command, sizeof(command),
                         IVSHMEM_CONFIG_COMMAND_OFFSET);
    if (transferred != (ssize_t)sizeof(command)) {
        close(fd);
        return IVSHMEM_ERR_IO;
    }
    /* Verify the sticky write so a later BAR access failure cannot hide a
     * silently ignored enable. */
    command = 0;
    transferred = pread(fd, &command, sizeof(command),
                        IVSHMEM_CONFIG_COMMAND_OFFSET);
    close(fd);
    if (transferred != (ssize_t)sizeof(command) ||
        (command & IVSHMEM_COMMAND_MEMORY_ENABLE) == 0) {
        return IVSHMEM_ERR_IO;
    }
    return IVSHMEM_OK;
}

static int is_mappable_bar(uint8_t bar)
{
    return bar == IVSHMEM_BAR_REGISTERS || bar == IVSHMEM_BAR_SHARED;
}

static const char *bar_resource_name(uint8_t bar)
{
    static __thread char name[32];

    snprintf(name, sizeof(name), "resource%u", (unsigned)bar);
    return name;
}

int ivshmem_bar_size(const struct ivshmem_device *dev, uint8_t bar,
                     size_t *size)
{
    char path[IVSHMEM_PATH_MAX];
    FILE *file;
    unsigned long start = 0;
    unsigned long end = 0;
    unsigned long flags = 0;
    unsigned long line_start = 0;
    unsigned long line_end = 0;
    unsigned long line_flags = 0;
    unsigned long index;

    if (dev == NULL || size == NULL || bar >= IVSHMEM_MAX_BARS) {
        return IVSHMEM_ERR_ARGS;
    }
    snprintf(path, sizeof(path), "%s/resource", dev->sysfs_dir);
    file = fopen(path, "r");
    if (file == NULL) {
        return IVSHMEM_ERR_IO;
    }
    for (index = 0; index <= bar; index++) {
        if (fscanf(file, "%lx %lx %lx", &line_start, &line_end,
                   &line_flags) != 3) {
            fclose(file);
            return IVSHMEM_ERR_IO;
        }
        start = line_start;
        end = line_end;
        flags = line_flags;
    }
    fclose(file);

    if (start == 0 && end == 0) {
        /* The BAR exists in the profile but was never assigned. */
        return IVSHMEM_ERR_IO;
    }
    if ((flags & IVSHMEM_RESOURCE_MEM_FLAG) == 0) {
        /* Only memory BARs map into the guest as shared windows. */
        return IVSHMEM_ERR_ARGS;
    }
    if (end < start) {
        return IVSHMEM_ERR_IO;
    }
    *size = (size_t)(end - start) + 1;
    return IVSHMEM_OK;
}

int ivshmem_map_bar(struct ivshmem_device *dev, uint8_t bar, void **map,
                    size_t *size)
{
    char path[IVSHMEM_PATH_MAX];
    struct ivshmem_mapping *mapping;
    size_t bar_bytes = 0;
    int result;

    if (dev == NULL || map == NULL || size == NULL) {
        return IVSHMEM_ERR_ARGS;
    }
    if (bar == IVSHMEM_BAR_MSIX) {
        /* Linux PCI/MSI-X owns the table and PBA; userspace must not map or
         * modify them behind the kernel's vector state. */
        return IVSHMEM_ERR_BACKEND;
    }
    if (!is_mappable_bar(bar)) {
        return IVSHMEM_ERR_ARGS;
    }
    mapping = &dev->bars[bar];
    if (mapping->map != NULL) {
        *map = mapping->map;
        *size = mapping->size;
        return IVSHMEM_OK;
    }

    result = ivshmem_bar_size(dev, bar, &bar_bytes);
    if (result != IVSHMEM_OK) {
        return result;
    }
    snprintf(path, sizeof(path), "%s/%s", dev->sysfs_dir,
             bar_resource_name(bar));
    mapping->fd = open(path, O_RDWR | O_SYNC);
    if (mapping->fd < 0) {
        return IVSHMEM_ERR_MMAP;
    }
    mapping->map = mmap(NULL, bar_bytes, PROT_READ | PROT_WRITE, MAP_SHARED,
                        mapping->fd, 0);
    if (mapping->map == MAP_FAILED) {
        close(mapping->fd);
        mapping->fd = -1;
        return IVSHMEM_ERR_MMAP;
    }
    mapping->size = bar_bytes;
    *map = mapping->map;
    *size = mapping->size;
    return IVSHMEM_OK;
}

static const struct ivshmem_mapping *register_mapping(
    const struct ivshmem_device *dev)
{
    if (dev == NULL) {
        return NULL;
    }
    if (dev->bars[IVSHMEM_BAR_REGISTERS].map == NULL) {
        return NULL;
    }
    return &dev->bars[IVSHMEM_BAR_REGISTERS];
}

uint32_t ivshmem_read_reg32(const struct ivshmem_device *dev, uint32_t offset)
{
    const struct ivshmem_mapping *mapping = register_mapping(dev);
    volatile const uint32_t *register_value;

    if (mapping == NULL || offset % sizeof(uint32_t) != 0 ||
        offset >= IVSHMEM_REG_PAGE_SIZE) {
        fprintf(stderr,
                "ivshmem: rejecting unaligned or unmapped BAR0 read at "
                "0x%08x\n",
                offset);
        return 0;
    }
    register_value = (volatile const uint32_t *)((const char *)mapping->map +
                                                 offset);
    return *register_value;
}

void ivshmem_write_reg32(const struct ivshmem_device *dev, uint32_t offset,
                         uint32_t value)
{
    const struct ivshmem_mapping *mapping = register_mapping(dev);
    volatile uint32_t *register_value;

    if (mapping == NULL || offset % sizeof(uint32_t) != 0 ||
        offset >= IVSHMEM_REG_PAGE_SIZE) {
        fprintf(stderr,
                "ivshmem: rejecting unaligned or unmapped BAR0 write at "
                "0x%08x\n",
                offset);
        return;
    }
    register_value =
        (volatile uint32_t *)((char *)mapping->map + offset);
    *register_value = value;
}

void *ivshmem_shared_memory(const struct ivshmem_device *dev, size_t *size)
{
    if (dev == NULL) {
        return NULL;
    }
    if (dev->bars[IVSHMEM_BAR_SHARED].map == NULL) {
        if (size != NULL) {
            *size = 0;
        }
        return NULL;
    }
    if (size != NULL) {
        *size = dev->bars[IVSHMEM_BAR_SHARED].size;
    }
    return dev->bars[IVSHMEM_BAR_SHARED].map;
}

static int is_uio_name(const char *name)
{
    const char *digit;

    if (strncmp(name, "uio", 3) != 0 || name[3] == '\0') {
        return 0;
    }
    for (digit = name + 3; *digit != '\0'; digit++) {
        if (*digit < '0' || *digit > '9') {
            return 0;
        }
    }
    return 1;
}

static int open_matching_uio(const struct ivshmem_device *dev,
                             const char *uio_class_root,
                             const char *device_root)
{
    char expected_device[IVSHMEM_PATH_MAX];
    char class_path[IVSHMEM_PATH_MAX];
    char resolved_device[IVSHMEM_PATH_MAX];
    char device_path[IVSHMEM_PATH_MAX];
    char match[256] = { 0 };
    struct dirent *entry;
    DIR *directory;

    if (realpath(dev->sysfs_dir, expected_device) == NULL) {
        return -1;
    }
    directory = opendir(uio_class_root);
    if (directory == NULL) {
        return -1;
    }
    while ((entry = readdir(directory)) != NULL) {
        if (!is_uio_name(entry->d_name)) {
            continue;
        }
        int written = snprintf(class_path, sizeof(class_path), "%s/%s/device",
                               uio_class_root, entry->d_name);

        if (written < 0 || (size_t)written >= sizeof(class_path) ||
            realpath(class_path, resolved_device) == NULL ||
            strcmp(resolved_device, expected_device) != 0) {
            continue;
        }
        if (match[0] != '\0') {
            closedir(directory);
            return -1;
        }
        if (strlen(entry->d_name) >= sizeof(match)) {
            closedir(directory);
            return -1;
        }
        memcpy(match, entry->d_name, strlen(entry->d_name) + 1);
    }
    closedir(directory);
    if (match[0] == '\0') {
        return -1;
    }
    {
        int written = snprintf(device_path, sizeof(device_path), "%s/%s",
                               device_root, match);

        if (written < 0 || (size_t)written >= sizeof(device_path)) {
            return -1;
        }
    }
    return open(device_path, O_RDWR | O_CLOEXEC);
}

int ivshmem_backend_open_at(struct ivshmem_device *dev,
                            enum ivshmem_backend_kind kind,
                            const char *uio_class_root,
                            const char *device_root,
                            struct ivshmem_backend **out)
{
    struct ivshmem_backend *backend;

    if (dev == NULL || out == NULL || uio_class_root == NULL ||
        device_root == NULL) {
        return IVSHMEM_ERR_ARGS;
    }
    *out = NULL;
    if (kind != IVSHMEM_BACKEND_POLLING &&
        kind != IVSHMEM_BACKEND_INTERRUPT) {
        return IVSHMEM_ERR_ARGS;
    }
    backend = calloc(1, sizeof(*backend));
    if (backend == NULL) {
        return IVSHMEM_ERR_NOMEM;
    }
    backend->dev = dev;
    backend->kind = kind;
    backend->interrupt_fd = -1;
    if (kind == IVSHMEM_BACKEND_INTERRUPT) {
        void *registers = NULL;
        size_t register_size = 0;

        if (ivshmem_map_bar(dev, IVSHMEM_BAR_REGISTERS, &registers,
                            &register_size) != IVSHMEM_OK ||
            register_size < IVSHMEM_REG_PAGE_SIZE) {
            free(backend);
            return IVSHMEM_ERR_BACKEND;
        }
        backend->interrupt_fd =
            open_matching_uio(dev, uio_class_root, device_root);
        if (backend->interrupt_fd < 0) {
            free(backend);
            return IVSHMEM_ERR_BACKEND;
        }
    }
    *out = backend;
    return IVSHMEM_OK;
}

int ivshmem_backend_open(struct ivshmem_device *dev,
                         enum ivshmem_backend_kind kind,
                         struct ivshmem_backend **out)
{
    return ivshmem_backend_open_at(dev, kind, IVSHMEM_UIO_CLASS,
                                   IVSHMEM_DEVICE_ROOT, out);
}

static long monotonic_ms(void)
{
    struct timespec now;

    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec * 1000 + now.tv_nsec / 1000000;
}

static void sleep_poll_interval(void)
{
    struct timespec pause = { .tv_sec = 0,
                              .tv_nsec = IVSHMEM_POLL_INTERVAL_MS * 1000000 };

    nanosleep(&pause, NULL);
}

static int wait_polling(struct ivshmem_backend *backend, int timeout_ms)
{
    long deadline = timeout_ms < 0 ? 0 : monotonic_ms() + timeout_ms;

    for (;;) {
        uint32_t status = ivshmem_read_reg32(backend->dev,
                                             IVSHMEM_REG_EVENT_STATUS);

        if (status & 1) {
            ivshmem_write_reg32(backend->dev, IVSHMEM_REG_EVENT_STATUS, 1);
            return 1;
        }
        if (timeout_ms >= 0 && monotonic_ms() >= deadline) {
            return 0;
        }
        sleep_poll_interval();
    }
}

static int wait_interrupt(struct ivshmem_backend *backend, int timeout_ms)
{
    struct pollfd ready = {
        .fd = backend->interrupt_fd,
        .events = POLLIN,
    };
    uint32_t event_count;
    uint32_t rearm = 1;
    int result;

    result = poll(&ready, 1, timeout_ms);
    if (result == 0) {
        return 0;
    }
    if (result < 0 || (ready.revents & POLLIN) == 0 ||
        read(backend->interrupt_fd, &event_count, sizeof(event_count)) !=
            (ssize_t)sizeof(event_count)) {
        return IVSHMEM_ERR_IO;
    }
    /* UIO masks the vector in its handler. Clear protocol state before
     * rearming so a new interrupt cannot race with stale Event Status. */
    ivshmem_write_reg32(backend->dev, IVSHMEM_REG_EVENT_STATUS, 1);
    if (write(backend->interrupt_fd, &rearm, sizeof(rearm)) !=
        (ssize_t)sizeof(rearm)) {
        return IVSHMEM_ERR_IO;
    }
    return 1;
}

int ivshmem_backend_wait_event(struct ivshmem_backend *be, int timeout_ms)
{
    if (be == NULL || be->dev == NULL) {
        return IVSHMEM_ERR_ARGS;
    }
    if (be->kind == IVSHMEM_BACKEND_INTERRUPT) {
        return wait_interrupt(be, timeout_ms);
    }
    return wait_polling(be, timeout_ms);
}

void ivshmem_backend_close(struct ivshmem_backend *be)
{
    if (be == NULL) {
        return;
    }
    if (be->interrupt_fd >= 0) {
        close(be->interrupt_fd);
    }
    free(be);
}
