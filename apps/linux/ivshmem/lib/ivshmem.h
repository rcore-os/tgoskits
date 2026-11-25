/*
 * Userspace adapter for the AxVisor ivshmem PCI profile.
 *
 * The adapter is the only device-access layer for Linux guests: discovery,
 * BAR mapping, register access, and event waiting all go through this API so
 * applications never hard-code a BDF, a BAR address, or a sysfs path
 * convention. The legacy QEMU VID:DID 1af4:1110 is retained only as a
 * compatibility alias. The AxVisor transport is identified by class
 * 0xff0000, revision 1, and its vendor-specific 0x09 capability carrying the
 * AXIV signature and protocol 0x4000. Revision 1 is compatible with the fixed
 * upstream uio_ivshmem probe; class and capability exclude classic QEMU. A
 * VID:DID-only match is never enough.
 * BAR0 is a 4 KiB page of aligned little-endian 32-bit registers: ID and
 * MAX_PEERS and the common/output section sizes are read-only; INTERRUPT_CTRL
 * enables notifications (bit 0); DOORBELL writes encode target peer in bits
 * 31:16 and vector in bits 15:0; STATE publishes this peer's value into
 * BAR2's read-only state table; EVENT_STATUS bit 0 is write-one-to-clear.
 * BAR1 is kernel-owned MSI-X; BAR2 is a prefetchable power-of-two window with a
 * 4 KiB read-only state table, an optional common section, and one page-aligned
 * output section per peer (writable only by its owner). The two size registers
 * are expressed in bytes. A zero output size means the layout is unavailable,
 * including legacy revision-1 devices, so callers must not infer it from
 * MAX_PEERS. BAR2 is mapped through sysfs resource2_wc so Linux uses its
 * write-combining path, which is Normal non-cacheable on AArch64.
 * The revision byte and AxVisor capability are checked before any of these
 * semantics are used. The reserved tail of the state-table page contains one
 * host-owned 64-bit reset/detach epoch per peer at 0x100 + peer * 8. Guests
 * only read these epochs and must use an acquire observation before accepting
 * Message V1 headers or queued payload after a peer restart.
 */
#ifndef IVSHMEM_H
#define IVSHMEM_H

#include <stddef.h>
#include <stdint.h>

/* AxVisor ivshmem identity; VID:DID alone remains a legacy QEMU alias. */
#define IVSHMEM_VENDOR_ID 0x1af4
#define IVSHMEM_DEVICE_ID 0x1110
#define IVSHMEM_CLASS 0xff0000
#define IVSHMEM_REVISION 0x01

/*
 * Conventional PCI capability list entries used by the transport.
 * The 0x09 capability is 36 bytes including the generic id/next header:
 *   0 id, 1 next, 2 length=36, 3 version=1, 4..8 "AXIV",
 *   8..10 protocol 0x4000, 10..12 reserved,
 *   then the 34-byte body's remaining fields:
 *   12..16 feature bits, 16..20 epoch base, 20..24 epoch stride,
 *   24..28 common size, 28..32 output size, 32..36 max peers.
 */
#define IVSHMEM_CAP_ID_VENDOR 0x09
#define IVSHMEM_TRANSPORT_CAP_LENGTH 0x24
#define IVSHMEM_TRANSPORT_CAP_VERSION 0x01
#define IVSHMEM_TRANSPORT_CAP_MAGIC 0x56495841u /* "AXIV" */
#define IVSHMEM_TRANSPORT_PROTOCOL 0x4000u
#define IVSHMEM_TRANSPORT_FEATURE_PEER_EPOCH 0x00000001u

/* BAR-relative register offsets (BAR0, little-endian, 32-bit only). */
#define IVSHMEM_REG_ID 0x00
#define IVSHMEM_REG_MAX_PEERS 0x04
#define IVSHMEM_REG_INTERRUPT_CTRL 0x08
#define IVSHMEM_REG_DOORBELL 0x0c
#define IVSHMEM_REG_STATE 0x10
#define IVSHMEM_REG_EVENT_STATUS 0x14
#define IVSHMEM_REG_COMMON_SIZE 0x18
#define IVSHMEM_REG_OUTPUT_SIZE 0x1c
#define IVSHMEM_REG_PAGE_SIZE 0x1000

/* BAR2-relative host-owned peer reset/detach epochs. */
#define IVSHMEM_STATE_EPOCH_OFFSET 0x100u
#define IVSHMEM_STATE_EPOCH_STRIDE 8u
#define IVSHMEM_MAX_PROFILE_PEERS 15u

#define IVSHMEM_BAR_REGISTERS 0
#define IVSHMEM_BAR_MSIX 1
#define IVSHMEM_BAR_SHARED 2

enum ivshmem_err {
    IVSHMEM_OK = 0,
    IVSHMEM_ERR_NOT_FOUND = -1, /* no matching device on the bus */
    IVSHMEM_ERR_AMBIGUOUS = -2, /* several matches; explicit BDF required */
    IVSHMEM_ERR_PROFILE = -3,   /* full AxVisor transport profile mismatch */
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
 * class, revision, and the AxVisor 0x09 capability before the handle is
 * returned. A device that matches the legacy VID:DID but not the full profile
 * fails with IVSHMEM_ERR_PROFILE instead of being skipped, so callers can
 * never bind to a device they have not been adapted for.
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
 *              BAR0 maps resource0 as Device memory. BAR2 maps
 *              resource2_wc as Normal non-cacheable memory on AArch64 and
 *              fails with IVSHMEM_ERR_MMAP when that file is unavailable;
 *              it never falls back to resource2. IVSHMEM_BAR_MSIX remains
 *              kernel-owned and returns IVSHMEM_ERR_BACKEND.
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

/**
 * Observes one peer's host-owned reset/detach epoch.
 *
 * BAR2 must already be mapped. The epoch is read from the read-only state
 * page with acquire ordering, so a changed nonzero value orders the
 * observation after the host's epoch publication. Callers retain the previous
 * value and discard stale Message V1 headers or queued data whenever the
 * observed epoch changes. Peer IDs outside the fixed 15-slot profile fail.
 *
 * @return IVSHMEM_OK, IVSHMEM_ERR_BACKEND when BAR2 is not mapped, or
 *         IVSHMEM_ERR_ARGS for an invalid peer/output pointer.
 */
int ivshmem_observe_peer_epoch(const struct ivshmem_device *dev,
                               uint16_t peer, uint64_t *epoch);

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
