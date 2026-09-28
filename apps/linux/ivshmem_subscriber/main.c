#define _POSIX_C_SOURCE 200809L
/* Linux peer 1/2: Message V1 broadcast reception and addressed reply. */
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "ivshmem.h"

#define PEERS 3u
#define CAP 15u
#define READY 1u
#define MAGIC 0x49564333u
#define BROADCAST 0xffffu
#define TIMEOUT_MS 30000L

static const char request[] = "hello ivshmem subscribers";
static const char reply[] = "ack";

struct header {
    _Atomic uint32_t magic, version, capacity, slot_size, tail;
    _Atomic uint32_t credit[15];
    _Atomic uint32_t reserved[44];
};
struct slot {
    _Atomic uint32_t seq, dst, len, flags;
    _Atomic uint32_t payload[60];
};
struct section {
    struct header header;
    struct slot slots[CAP];
};
_Static_assert(sizeof(struct header) == 256, "ivshmem metadata slot");
_Static_assert(sizeof(struct slot) == 256, "ivshmem data slot");
_Static_assert(sizeof(struct section) == 4096, "ivshmem output page");

static void fail(const char *why) {
    fprintf(stderr, "IVSHMEM_SUBSCRIBER_FAILED %s\n", why);
    exit(1);
}
static long now_ms(void) {
    struct timespec t;
    if (clock_gettime(CLOCK_MONOTONIC, &t) != 0) fail("clock");
    return t.tv_sec * 1000L + t.tv_nsec / 1000000;
}
static uint32_t next(uint32_t n) { return n == UINT32_MAX - 1 ? 0 : n + 1; }
static uint32_t word(const unsigned char *p) {
    return (uint32_t)p[0] | (uint32_t)p[1] << 8 |
           (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}
static void put32(unsigned char *p, uint32_t n) {
    for (size_t i = 0; i < 4; i++) p[i] = (unsigned char)(n >> (8 * i));
}
static void read_slot(const struct slot *slot, unsigned char *out, size_t len) {
    for (size_t i = 0; i < (len + 3) / 4; i++) {
        uint32_t n = atomic_load_explicit(&slot->payload[i], memory_order_relaxed);
        for (size_t b = 0; b < 4 && i * 4 + b < len; b++)
            out[i * 4 + b] = (unsigned char)(n >> (8 * b));
    }
}
static void write_slot(struct slot *slot, const unsigned char *in, size_t len) {
    unsigned char tmp[4] = {0};
    for (size_t i = 0; i < (len + 3) / 4; i++) {
        memset(tmp, 0, sizeof(tmp));
        size_t count = len - i * 4 < 4 ? len - i * 4 : 4;
        memcpy(tmp, in + i * 4, count);
        atomic_store_explicit(&slot->payload[i], word(tmp), memory_order_relaxed);
    }
}

int main(void) {
    struct ivshmem_device *dev = NULL;
    struct ivshmem_backend *backend = NULL;
    void *shared = NULL;
    size_t shared_size = 0, register_size = 0;
    void *registers = NULL;
    if (ivshmem_find_device(NULL, &dev) != IVSHMEM_OK ||
        ivshmem_enable_device(dev) != IVSHMEM_OK ||
        ivshmem_map_bar(dev, IVSHMEM_BAR_REGISTERS, &registers, &register_size) != IVSHMEM_OK ||
        ivshmem_map_bar(dev, IVSHMEM_BAR_SHARED, &shared, &shared_size) != IVSHMEM_OK ||
        ivshmem_backend_open(dev, IVSHMEM_BACKEND_POLLING, &backend) != IVSHMEM_OK)
        fail("PCI discovery or BAR mapping");
    (void)registers;
    if (register_size < IVSHMEM_REG_PAGE_SIZE || shared_size < 4 * 4096 ||
        ivshmem_read_reg32(dev, IVSHMEM_REG_MAX_PEERS) != PEERS)
        fail("unexpected ivshmem profile");
    unsigned id = ivshmem_read_reg32(dev, IVSHMEM_REG_ID);
    if (id != 1 && id != 2) fail("expected Linux peer 1 or 2");
    _Atomic uint32_t *states = shared;
    struct section *sections = (void *)((unsigned char *)shared + 4096);
    struct section *own = &sections[id];
    long deadline = now_ms() + TIMEOUT_MS;
    while (atomic_load_explicit(&states[0], memory_order_acquire) != READY) {
        if (now_ms() >= deadline) fail("ArceOS publisher not ready");
    }
    if (atomic_load_explicit(&sections[0].header.magic, memory_order_acquire) != MAGIC)
        fail("publisher section is not initialized");
    /* The publisher waits for both Linux peers before it starts writing. */
    for (size_t p = 0; p < PEERS; p++) {
        uint32_t head = p == id ? 0 : atomic_load_explicit(&sections[p].header.tail, memory_order_acquire);
        atomic_store_explicit(&own->header.credit[p], head, memory_order_relaxed);
    }
    for (size_t s = 0; s < CAP; s++) atomic_store(&own->slots[s].seq, UINT32_MAX);
    atomic_store(&own->header.tail, 0);
    atomic_store(&own->header.version, 1);
    atomic_store(&own->header.capacity, CAP);
    atomic_store(&own->header.slot_size, 256);
    atomic_store_explicit(&own->header.magic, MAGIC, memory_order_release);
    atomic_thread_fence(memory_order_seq_cst);
    ivshmem_write_reg32(dev, IVSHMEM_REG_STATE, READY);

    struct section *pub = &sections[0];
    unsigned char payload[240];
    int received = 0;
    while (!received) {
        if (now_ms() >= deadline) fail("broadcast request timed out");
        uint32_t cursor = atomic_load_explicit(&own->header.credit[0], memory_order_relaxed);
        uint32_t tail = atomic_load_explicit(&pub->header.tail, memory_order_acquire);
        if (cursor == tail) {
            int event = ivshmem_backend_wait_event(backend, 1);
            if (event < 0) fail("doorbell wait");
            continue;
        }
        struct slot *slot = &pub->slots[cursor % CAP];
        if (atomic_load_explicit(&slot->seq, memory_order_acquire) != cursor)
            fail("message sequence mismatch");
        uint32_t len = atomic_load(&slot->len);
        uint32_t dst = atomic_load(&slot->dst);
        if (len > sizeof(payload) || atomic_load(&slot->flags) != 0)
            fail("invalid slot metadata");
        read_slot(slot, payload, len);
        atomic_store_explicit(&own->header.credit[0], next(cursor), memory_order_release);
        if (dst != id && dst != BROADCAST) continue;
        if (dst != BROADCAST || len != 24 + sizeof(request) - 1 ||
            payload[0] != 1 || payload[1] != 3 || payload[2] != 24 || payload[3] != 0 ||
            word(payload + 4) != sizeof(request) - 1 || word(payload + 8) != 1 ||
            word(payload + 12) != 0 || word(payload + 16) != sizeof(request) - 1 ||
            word(payload + 20) != 0 ||
            memcmp(payload + 24, request, sizeof(request) - 1) != 0)
            fail("invalid Message V1 broadcast");
        received = 1;
    }

    /* Both Linux peers publish their own acknowledgements in separate rings. */
    uint32_t tail = atomic_load(&own->header.tail);
    struct slot *slot = &own->slots[tail % CAP];
    memset(payload, 0, sizeof(payload));
    payload[0] = 1; payload[1] = 3;
    payload[2] = 24;
    put32(payload + 4, sizeof(reply) - 1);
    put32(payload + 8, 1);
    put32(payload + 16, sizeof(reply) - 1);
    memcpy(payload + 24, reply, sizeof(reply) - 1);
    write_slot(slot, payload, 24 + sizeof(reply) - 1);
    atomic_store(&slot->dst, 0);
    atomic_store(&slot->len, 24 + sizeof(reply) - 1);
    atomic_store(&slot->flags, 0);
    atomic_store_explicit(&slot->seq, tail, memory_order_release);
    atomic_store_explicit(&own->header.tail, next(tail), memory_order_release);
    atomic_thread_fence(memory_order_seq_cst);
    ivshmem_write_reg32(dev, IVSHMEM_REG_DOORBELL, 0);
    printf("IVSHMEM_SUBSCRIBER_PASSED peer=%u\n", id);
    fflush(stdout);
    ivshmem_backend_close(backend);
    ivshmem_device_close(dev);
    return 0;
}
