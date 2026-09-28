/*
 * Internal adapter state shared by the ivshmem library translation units.
 *
 * Nothing here is part of the public ABI: applications only see the opaque
 * handles declared in ivshmem.h. The device handle owns every mapping, so
 * closing it releases all BARs at once.
 */
#ifndef IVSHMEM_INTERNAL_H
#define IVSHMEM_INTERNAL_H

#include <stddef.h>
#include <stdint.h>

#include "ivshmem.h"

/* PCI config-space byte holding the device revision (PCI 3.0, offset 0x08). */
#define IVSHMEM_CONFIG_REVISION_OFFSET 0x08

/* PCI config-space Command register (offset 0x04) and its Memory Space
 * Enable bit; the polling adapter turns it on explicitly. */
#define IVSHMEM_CONFIG_COMMAND_OFFSET 0x04
#define IVSHMEM_COMMAND_MEMORY_ENABLE 0x0002u

/* IORESOURCE_MEM flag published in the third column of sysfs `resource`. */
#define IVSHMEM_RESOURCE_MEM_FLAG 0x00000200

#define IVSHMEM_MAX_BARS 6

/* Maximum sysfs path length the adapter handles; snprintf keeps every path
 * truncated-but-safe, and GCC proves the copies below cannot truncate. */
#define IVSHMEM_PATH_MAX 4096

/* The device directory leaves room for a `/resourceN` suffix inside one
 * IVSHMEM_PATH_MAX buffer, which also keeps the size provable for GCC's
 * -Wformat-truncation analysis. */
#define IVSHMEM_SYSFS_DIR_MAX 4000

/* One mapped BAR: the mmap stays valid until the owning device closes. */
struct ivshmem_mapping {
    int fd;
    void *map;
    size_t size;
};

struct ivshmem_device {
    /* Canonical BDF, e.g. "0000:00:01.0"; used for diagnostics only. */
    char bdf[16];
    /* sysfs device directory, e.g. "/sys/bus/pci/devices/0000:00:01.0". */
    char sysfs_dir[IVSHMEM_SYSFS_DIR_MAX];
    struct ivshmem_mapping bars[IVSHMEM_MAX_BARS];
};

/**
 * Maps the sysfs resource file of one BAR into a guest path.
 *
 * The devices root is injectable so the host-side unit tests can run the
 * discovery logic against a fixture tree; production callers always pass
 * the real sysfs mount.
 */
int ivshmem_find_at(const char *devices_root, const char *bdf,
                    struct ivshmem_device **out);

/**
 * Reads one BAR's size from the sysfs `resource` table.
 *
 * @param dev    Verified device handle.
 * @param bar    BAR index (0-5).
 * @param size   Receives the BAR size in bytes.
 * @return IVSHMEM_OK, or a negative ivshmem_err value.
 */
int ivshmem_bar_size(const struct ivshmem_device *dev, uint8_t bar,
                     size_t *size);

/**
 * Opens an event backend through injectable UIO roots.
 *
 * Production callers use `/sys/class/uio` and `/dev`; host tests supply a
 * fixture class and an fd-backed device node while exercising the same
 * discovery and wait implementation.
 */
int ivshmem_backend_open_at(struct ivshmem_device *dev,
                            enum ivshmem_backend_kind kind,
                            const char *uio_class_root,
                            const char *device_root,
                            struct ivshmem_backend **out);

#endif /* IVSHMEM_INTERNAL_H */
