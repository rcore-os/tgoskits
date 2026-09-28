/*
 * Userspace adapter for the AxVisor ivshmem PCI profile.
 *
 * The adapter is the only device-access layer for Linux guests: discovery,
 * BAR mapping, register access, and event waiting all go through this API so
 * applications never hard-code a BDF, a BAR address, or a sysfs path
 * convention. Revision 1 identifies PCI function 1af4:1110, class 0x050000.
 * BAR0 is a 4 KiB page of aligned little-endian 32-bit registers: ID and
 * MAX_PEERS are read-only; INTERRUPT_CTRL enables notifications (bit 0);
 * DOORBELL writes encode target peer in bits 31:16 and vector in bits 15:0;
 * STATE publishes this peer's value into BAR2's read-only state table;
 * EVENT_STATUS bit 0 is write-one-to-clear. BAR1 is kernel-owned MSI-X;
 * BAR2 is 64 KiB with a 4 KiB read-only state table, followed by one
 * page-aligned output section per peer (writable only by its owner).
 * The revision byte is checked before any of these semantics are used.
 */
#ifndef IVSHMEM_H
#define IVSHMEM_H

#include <stddef.h>
#include <stdint.h>

/* AxVisor ivshmem profile identity; callers must re-verify revision. */
#define IVSHMEM_VENDOR_ID 0x1af4
#define IVSHMEM_DEVICE_ID 0x1110
#define IVSHMEM_CLASS 0x050000
#define IVSHMEM_REVISION 0x01

/* BAR-relative register offsets (BAR0, little-endian, 32-bit only). */
#define IVSHMEM_REG_ID 0x00
#define IVSHMEM_REG_MAX_PEERS 0x04
#define IVSHMEM_REG_INTERRUPT_CTRL 0x08
#define IVSHMEM_REG_DOORBELL 0x0c
#define IVSHMEM_REG_STATE 0x10
#define IVSHMEM_REG_EVENT_STATUS 0x14
#define IVSHMEM_REG_PAGE_SIZE 0x1000

#define IVSHMEM_BAR_REGISTERS 0
#define IVSHMEM_BAR_MSIX 1
#define IVSHMEM_BAR_SHARED 2

enum ivshmem_err {
    IVSHMEM_OK = 0,
    IVSHMEM_ERR_NOT_FOUND = -1, /* no matching device on the bus */
    IVSHMEM_ERR_AMBIGUOUS = -2, /* several matches; explicit BDF required */
    IVSHMEM_ERR_PROFILE = -3,   /* identity or revision mismatch */
    IVSHMEM_ERR_IO = -4,        /* sysfs/procfs/devtmpfs access failed */
    IVSHMEM_ERR_MMAP = -5,      /* resource mmap failed */
    IVSHMEM_ERR_BACKEND = -6,   /* requested backend is unavailable */
    IVSHMEM_ERR_TIMEOUT = -7,   /* wait_event timed out */
    IVSHMEM_ERR_NOMEM = -8,
    IVSHMEM_ERR_ARGS = -9, /* invalid argument */
};

/* Returns a stable human-readable description of one ivshmem_err value. */
const char *ivshmem_strerror(int err);

struct ivshmem_device;
struct ivshmem_backend;

/**
 * Finds one AxVisor ivshmem PCI function.
 *
 * @param bdf  Explicit BDF such as "0000:00:01.0", or NULL to auto-detect.
 * @param out  Receives an opaque device handle on success.
 * @return IVSHMEM_OK, or a negative ivshmem_err value.
 *
 * Auto-detect requires exactly one match: several matches return
 * IVSHMEM_ERR_AMBIGUOUS, no match returns IVSHMEM_ERR_NOT_FOUND. Both
 * explicit and auto-detected devices are verified against vendor, device,
 * class and revision before the handle is returned. A device that matches
 * the ivshmem identity but not the profile revision fails with
 * IVSHMEM_ERR_PROFILE instead of being skipped, so callers can never bind
 * to a device they have not been adapted for.
 */
int ivshmem_find_device(const char *bdf, struct ivshmem_device **out);

/**
 * Releases one device handle and all mappings created from it.
 * Passing NULL is a no-op.
 */
void ivshmem_device_close(struct ivshmem_device *dev);

/**
 * Enables the device's PCI memory decoding.
 *
 * The polling path runs without a kernel driver, so nothing else sets the
 * Command register's Memory Space Enable bit; without it the function never
 * decodes BAR accesses. Call this once after discovery, before mapping.
 *
 * @param dev  Verified device handle.
 * @return IVSHMEM_OK, or a negative ivshmem_err value.
 */
int ivshmem_enable_device(struct ivshmem_device *dev);

/**
 * Maps one memory BAR of the device.
 *
 * @param bar   One of IVSHMEM_BAR_REGISTERS / IVSHMEM_BAR_SHARED.
 *              IVSHMEM_BAR_MSIX remains kernel-owned and returns
 *              IVSHMEM_ERR_BACKEND.
 * @param map   Receives the mapping base pointer.
 * @param size  Receives the mapping size in bytes.
 * @return IVSHMEM_OK or a negative error; the mapping stays valid until
 *         ivshmem_device_close().
 */
int ivshmem_map_bar(struct ivshmem_device *dev, uint8_t bar, void **map,
                    size_t *size);

/** Reads one aligned 32-bit BAR0 register; unaligned offsets read as 0. */
uint32_t ivshmem_read_reg32(const struct ivshmem_device *dev,
                            uint32_t offset);

/** Writes one aligned 32-bit BAR0 register; invalid offsets are ignored. */
void ivshmem_write_reg32(const struct ivshmem_device *dev, uint32_t offset,
                         uint32_t value);

/** Returns the shared-memory mapping and its size; NULL on error. */
void *ivshmem_shared_memory(const struct ivshmem_device *dev, size_t *size);

enum ivshmem_backend_kind {
    IVSHMEM_BACKEND_POLLING = 0,
    IVSHMEM_BACKEND_INTERRUPT = 1,
};

/**
 * Opens one event backend on the device.
 *
 * POLLING reads BAR0 Event Status through the PCI resource mapping.
 * INTERRUPT requires the uio_ivshmem binding and returns
 * IVSHMEM_ERR_BACKEND when /dev/uioN or the driver binding is missing.
 * There is no fallback: the caller decides which backend to open.
 */
int ivshmem_backend_open(struct ivshmem_device *dev,
                         enum ivshmem_backend_kind kind,
                         struct ivshmem_backend **out);

/**
 * Waits until one event is pending or the timeout expires.
 *
 * POLLING: polls Event Status bit 0; on return 1 the bit has been cleared
 * (write-1-to-clear) so the next call can observe the following event.
 * INTERRUPT: reads the 4-byte event count from /dev/uioN, clears Event
 * Status, then re-enables the vector by writing 1 to the UIO fd
 * (`irqcontrol`).
 *
 * @param timeout_ms  Milliseconds; a negative value waits indefinitely.
 * @return 1 when an event was observed, 0 on timeout, negative ivshmem_err
 *         on IO failure.
 */
int ivshmem_backend_wait_event(struct ivshmem_backend *be, int timeout_ms);

/** Closes one backend; NULL is a no-op. */
void ivshmem_backend_close(struct ivshmem_backend *be);

#endif /* IVSHMEM_H */
