#define _DEFAULT_SOURCE
#define _POSIX_C_SOURCE 200809L

/*
 * ivshmem shared-memory smoke scenario.
 *
 * This program owns only payload, checksum, marker, and timeout diagnostics;
 * every device interaction goes through the libivshmem adapter so the smoke
 * test never hard-codes a BDF, a BAR address, or a sysfs path.
 *
 * Output contract (frozen with the shared QEMU case):
 *   success: "ivshmem <backend> pass"
 *   failure: "ivshmem <backend> failed <step>: <detail>"
 *   progress: "ivshmem checkpoint <name>"
 *
 * The polling handshake exercises discovery without a fixed BDF, mapping
 * without absolute addresses, profile registers, a BAR2 payload round-trip,
 * and the doorbell/Event Status path: the peer rings its own doorbell and
 * waits through the polling backend twice (post-clear re-pend), then
 * verifies that an unsupported vector produces no event.
 *
 * With --cross-peer the smoke additionally runs the dual-peer handshake:
 * the initiator (peer 0) publishes a request mailbox in its own output
 * section (readable by the responder through the per-peer output
 * permissions), publishes its handshake state and rings the responder's
 * doorbell; the responder validates the request, publishes its reply and
 * rings back. The initiator's final "ivshmem polling pass" therefore
 * implies a completed cross-peer round trip; the responder prints the
 * distinct "ivshmem polling relay pass" instead.
 *
 * With --three-peer, peer 0 directs one request to peer 1 while peer 2
 * observes its Event Status and fails on any crosstalk. Peer 0 emits the
 * unique "ivshmem polling three-peer pass" marker only after validating all
 * three state-table entries and output-section mailboxes.
 *
 * Every mode additionally proves the F5/F6 write isolation: a write to the
 * state table and to a non-owner output section must fault into the guest
 * (SIGBUS or SIGSEGV from the injected data abort), leave the target bytes
 * unchanged, and let the VM keep running.
 */
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "ivshmem.h"

/* Payload lives inside this peer's output section. The frozen F5 BAR2
 * layout gives every peer one 28 KiB output page-range starting after the
 * state-table page (peer N: 0x1000 + N * 0x7000), so the smoke writes only
 * bytes it owns. */
#define SMOKE_OUTPUT_SECTION_BASE 0x1000
#define SMOKE_OUTPUT_SECTION_STRIDE 0x7000
#define SMOKE_PAYLOAD_SIZE 0x100

/* Cross-peer mailbox magic ("IVCP") and the BAR0 state values each side
 * publishes for the handshake; the remote peer reads them from the state
 * table (F4 remote-observation evidence). */
#define SMOKE_MAILBOX_MAGIC 0x49564350u
#define SMOKE_HANDSHAKE_STATE_SELF 0x00010002u
#define SMOKE_HANDSHAKE_STATE_INITIATOR 0x00010003u
#define SMOKE_HANDSHAKE_STATE_RESPONDER 0x00010004u
#define SMOKE_HANDSHAKE_STATE_READY 0x00010005u

/* Three-peer protocol state. */
#define SMOKE_THREE_OUTPUT_SECTION_STRIDE 0x5000
#define SMOKE_THREE_STATE_COORDINATOR 0x00020000u
#define SMOKE_THREE_STATE_TARGET_READY 0x00020001u
#define SMOKE_THREE_STATE_TARGET_RECEIVED 0x00020011u
#define SMOKE_THREE_STATE_TARGET_REPLIED 0x00020012u
#define SMOKE_THREE_STATE_OBSERVER_ARMED 0x00020020u
#define SMOKE_THREE_STATE_OBSERVER_CLEAN 0x00020021u

/* Guest scheduling is not part of the device contract: the handshake waits
 * long enough for the other guest to reach the smoke, and every wait
 * failure carries a distinct step name. */
#define SMOKE_HANDSHAKE_TIMEOUT_MS 30000

static const char *selected_backend = "unknown";

static void checkpoint(const char *name)
{
    printf("ivshmem checkpoint %s\n", name);
    fflush(stdout);
}

static void fail(const char *step, const char *detail)
{
    printf("ivshmem %s failed %s: %s\n", selected_backend, step, detail);
    fflush(stdout);
    exit(1);
}

static void fail_err(const char *step, int err)
{
    fail(step, ivshmem_strerror(err));
}

static void usage(const char *program)
{
    fprintf(stderr,
            "usage: %s --backend polling|interrupt [--cross-peer|--three-peer] "
            "[--bdf <BDF>]\n",
            program);
}

struct options {
    const char *bdf;
    enum ivshmem_backend_kind backend;
    int cross_peer;
    int three_peer;
};

static void parse_options(int argc, char **argv, struct options *options)
{
    int index;

    memset(options, 0, sizeof(*options));
    options->backend = IVSHMEM_BACKEND_POLLING;
    selected_backend = "polling";
    for (index = 1; index < argc; index++) {
        if (strcmp(argv[index], "--backend") == 0 && index + 1 < argc) {
            index++;
            if (strcmp(argv[index], "polling") == 0) {
                options->backend = IVSHMEM_BACKEND_POLLING;
                selected_backend = "polling";
            } else if (strcmp(argv[index], "interrupt") == 0) {
                options->backend = IVSHMEM_BACKEND_INTERRUPT;
                selected_backend = "interrupt";
            } else {
                fail("backend", "backend must be polling or interrupt");
            }
        } else if (strcmp(argv[index], "--bdf") == 0 && index + 1 < argc) {
            index++;
            options->bdf = argv[index];
        } else if (strcmp(argv[index], "--cross-peer") == 0) {
            options->cross_peer = 1;
        } else if (strcmp(argv[index], "--three-peer") == 0) {
            options->three_peer = 1;
        } else {
            usage(argv[0]);
            fail("args", "unrecognized command line");
        }
    }
    if (options->cross_peer && options->three_peer) {
        fail("args", "cross-peer and three-peer modes are mutually exclusive");
    }
    if (options->three_peer &&
        options->backend != IVSHMEM_BACKEND_POLLING) {
        fail("backend", "three-peer routing evidence requires polling mode");
    }
}

static uint32_t payload_checksum(const uint8_t *payload, size_t size)
{
    uint32_t checksum = 0x49565348u;
    size_t index;

    for (index = 0; index < size; index++) {
        checksum = checksum * 31u + payload[index];
    }
    return checksum;
}

static void exchange_payload(void *shared, uint32_t peer_id,
                             size_t output_stride)
{
    size_t payload_offset =
        SMOKE_OUTPUT_SECTION_BASE + (size_t)peer_id * output_stride;
    uint8_t payload[SMOKE_PAYLOAD_SIZE];
    volatile uint8_t *remote = (volatile uint8_t *)shared + payload_offset;
    uint32_t written_checksum;
    uint32_t readback_checksum;
    size_t index;

    for (index = 0; index < sizeof(payload); index++) {
        payload[index] = (uint8_t)(index * 7 + 0x5a);
    }
    written_checksum = payload_checksum(payload, sizeof(payload));

    for (index = 0; index < sizeof(payload); index++) {
        remote[index] = payload[index];
    }
    for (index = 0; index < sizeof(payload); index++) {
        payload[index] = remote[index];
    }
    readback_checksum = payload_checksum(payload, sizeof(payload));
    if (written_checksum != readback_checksum) {
        fail("shared-memory", "BAR2 payload checksum mismatch");
    }
    printf("ivshmem checkpoint payload offset=%zx\n", payload_offset);
    fflush(stdout);
}

/* Cross-peer mailbox exchanged through the two output sections: each peer
 * writes only its own section and reads the other's, matching the F5
 * ownership rules. */
struct smoke_mailbox {
    uint32_t magic;
    uint32_t checksum;
    uint32_t reserved[2];
    uint8_t payload[SMOKE_PAYLOAD_SIZE];
};

static volatile struct smoke_mailbox *section_mailbox(void *shared,
                                                      uint32_t peer_id,
                                                      size_t output_stride)
{
    size_t offset =
        SMOKE_OUTPUT_SECTION_BASE + (size_t)peer_id * output_stride;

    return (volatile struct smoke_mailbox *)((volatile uint8_t *)shared + offset);
}

static uint32_t mailbox_payload_checksum(
    const volatile struct smoke_mailbox *mailbox)
{
    uint8_t payload[SMOKE_PAYLOAD_SIZE];
    size_t index;

    for (index = 0; index < sizeof(payload); index++) {
        payload[index] = mailbox->payload[index];
    }
    return payload_checksum(payload, sizeof(payload));
}

static void fill_mailbox(volatile struct smoke_mailbox *mailbox, uint32_t seed)
{
    size_t index;

    for (index = 0; index < sizeof(mailbox->payload); index++) {
        mailbox->payload[index] = (uint8_t)(index * 7 + seed);
    }
    mailbox->magic = SMOKE_MAILBOX_MAGIC;
    mailbox->checksum = mailbox_payload_checksum(mailbox);
}

static void validate_mailbox(const volatile struct smoke_mailbox *mailbox,
                             const char *step)
{
    if (mailbox->magic != SMOKE_MAILBOX_MAGIC) {
        fail(step, "cross-peer mailbox magic mismatch");
    }
    if (mailbox->checksum != mailbox_payload_checksum(mailbox)) {
        fail(step, "cross-peer payload checksum mismatch");
    }
}

static void wait_remote_state(const volatile uint32_t *state_table,
                              uint32_t peer_id, uint32_t expected,
                              const char *step)
{
    uint32_t spins;
    int attempt;

    /* The remote peer publishes its handshake state through BAR0 before it
     * rings the doorbell, but Normal-memory stores do not order against the
     * subsequent Device write, so poll briefly instead of assuming the
     * value is visible once the event arrives. The state write only trails
     * the event by one coherency transaction, so the loop normally exits on
     * the first observation. */
    for (attempt = 0; attempt < 100; attempt++) {
        if (state_table[peer_id] == expected) {
            return;
        }
        for (spins = 0; spins < 100000u; spins++) {
            /* Compiler barrier: keep the observation loop real. */
            __asm__ volatile ("" ::: "memory");
        }
    }
    fail(step, "remote peer state did not reach the handshake value");
}

static void wait_remote_state_long(const volatile uint32_t *state_table,
                                   uint32_t peer_id, uint32_t expected,
                                   const char *step);

static void cross_peer_exchange(void *shared, uint32_t peer_id,
                                struct ivshmem_device *dev,
                                struct ivshmem_backend *backend)
{
    const volatile uint32_t *state_table = (const volatile uint32_t *)shared;
    volatile struct smoke_mailbox *own =
        section_mailbox(shared, peer_id, SMOKE_OUTPUT_SECTION_STRIDE);
    uint32_t target = peer_id ^ 1u;
    volatile struct smoke_mailbox *remote =
        section_mailbox(shared, target, SMOKE_OUTPUT_SECTION_STRIDE);
    int wait_result;

    if (peer_id == 0) {
        /* Interrupt delivery is not valid until the responder has opened its
         * event backend and completed MSI-X/UIO setup. Polling peers use the
         * same readiness state so the protocol has one deterministic start. */
        wait_remote_state_long(state_table, target,
                               SMOKE_HANDSHAKE_STATE_READY,
                               "cross-peer-ready");

        /* Initiator: publish the request, ring the responder, then validate
         * the responder's published state and reply. */
        fill_mailbox(own, 0x5a);
        ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                            SMOKE_HANDSHAKE_STATE_INITIATOR);
        /* The doorbell is a Device write and does not order the prior
         * Normal-memory mailbox and state stores. */
        __sync_synchronize();
        ivshmem_write_reg32(dev, IVSHMEM_REG_DOORBELL, (target << 16) | 0u);
        checkpoint("cross-peer-request");

        wait_result =
            ivshmem_backend_wait_event(backend, SMOKE_HANDSHAKE_TIMEOUT_MS);
        if (wait_result != 1) {
            fail("cross-peer", wait_result == 0 ? "reply event timed out"
                                                : "event wait failed");
        }
        wait_remote_state(state_table, target,
                          SMOKE_HANDSHAKE_STATE_RESPONDER, "cross-peer");
        validate_mailbox(remote, "cross-peer");
        checkpoint("cross-peer-reply");
    } else {
        /* Responder: publish readiness only after the selected event backend
         * is open. The first event can therefore only be the initiator's
         * request. Self-doorbell tests run after the exchange because a
         * pending self event would merge with the request event. */
        ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                            SMOKE_HANDSHAKE_STATE_READY);
        __sync_synchronize();
        checkpoint("cross-peer-ready");

        wait_result =
            ivshmem_backend_wait_event(backend, SMOKE_HANDSHAKE_TIMEOUT_MS);
        if (wait_result != 1) {
            fail("cross-peer", wait_result == 0 ? "request event timed out"
                                                : "event wait failed");
        }
        wait_remote_state(state_table, target,
                          SMOKE_HANDSHAKE_STATE_INITIATOR, "cross-peer");
        validate_mailbox(remote, "cross-peer");

        fill_mailbox(own, 0xa5);
        ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                            SMOKE_HANDSHAKE_STATE_RESPONDER);
        __sync_synchronize();
        ivshmem_write_reg32(dev, IVSHMEM_REG_DOORBELL, (target << 16) | 0u);
        checkpoint("cross-peer-relay");
    }
}

static sigjmp_buf deny_jump;
static volatile sig_atomic_t deny_armed;

static void deny_signal_handler(int signal)
{
    if (deny_armed) {
        deny_armed = 0;
        siglongjmp(deny_jump, 1);
    }
    _exit(128 + signal);
}

static void install_deny_signal_handlers(void)
{
    struct sigaction action;

    memset(&action, 0, sizeof(action));
    action.sa_handler = deny_signal_handler;
    sigemptyset(&action.sa_mask);
    action.sa_flags = SA_NODEFER;
    if (sigaction(SIGBUS, &action, NULL) != 0 ||
        sigaction(SIGSEGV, &action, NULL) != 0) {
        fail("deny-write", "failed to install signal handlers");
    }
}

/* A write to a read-only stage-2 section must fault back into the guest
 * (SIGBUS or SIGSEGV from the injected synchronous external abort) instead
 * of reaching the backing: the target bytes stay unchanged and the VM keeps
 * running. */
static void expect_denied_write(volatile uint32_t *target, const char *step)
{
    uint32_t before = *target;

    if (sigsetjmp(deny_jump, 1) == 0) {
        deny_armed = 1;
        __sync_synchronize();
        *target = 0xdeadbeefu;
        deny_armed = 0;
        fail(step, "a read-only stage-2 section accepted a write");
    }
    deny_armed = 0;
    __sync_synchronize();
    if (*target != before) {
        fail(step, "a denied write still modified the shared bytes");
    }
}

static void deny_write_tests(void *shared, uint32_t peer_id,
                             uint32_t max_peers, size_t output_stride)
{
    volatile uint32_t *state_entry = (volatile uint32_t *)shared + peer_id;
    uint32_t remote_peer = (peer_id + 1) % max_peers;
    volatile uint32_t *remote_output =
        (volatile uint32_t *)((volatile uint8_t *)shared +
                              SMOKE_OUTPUT_SECTION_BASE +
                              (size_t)remote_peer * output_stride);

    install_deny_signal_handlers();
    expect_denied_write(state_entry, "deny-write-state");
    checkpoint("deny-write-state");
    expect_denied_write(remote_output, "deny-write-output");
    checkpoint("deny-write-output");
}

static void self_doorbell_tests(struct ivshmem_device *dev,
                                struct ivshmem_backend *backend,
                                uint32_t peer_id)
{
    int wait_result;

    /* Ring the doorbell for this endpoint itself: the target Event Status
     * must pend, the polling backend must clear it via W1C, and a second
     * doorbell must pend again. */
    ivshmem_write_reg32(dev, IVSHMEM_REG_DOORBELL,
                        (peer_id << 16) | 0u);
    wait_result = ivshmem_backend_wait_event(backend, 5000);
    if (wait_result != 1) {
        fail("doorbell", wait_result == 0 ? "first event timed out"
                                          : "event wait failed");
    }
    if ((ivshmem_read_reg32(dev, IVSHMEM_REG_EVENT_STATUS) & 1) != 0) {
        fail("doorbell", "event status was not cleared by the wait");
    }
    checkpoint("doorbell-first");

    ivshmem_write_reg32(dev, IVSHMEM_REG_DOORBELL, (peer_id << 16) | 0u);
    wait_result = ivshmem_backend_wait_event(backend, 5000);
    if (wait_result != 1) {
        fail("doorbell", wait_result == 0 ? "second event timed out"
                                          : "event wait failed");
    }
    checkpoint("doorbell-second");
}

static long monotonic_ms(void)
{
    struct timespec now;

    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
        fail("clock", "clock_gettime failed");
    }
    return now.tv_sec * 1000L + now.tv_nsec / 1000000L;
}

static void sleep_one_millisecond(void)
{
    const struct timespec delay = {
        .tv_sec = 0,
        .tv_nsec = 1000000L,
    };

    nanosleep(&delay, NULL);
}

static void wait_remote_state_long(const volatile uint32_t *state_table,
                                   uint32_t peer_id, uint32_t expected,
                                   const char *step)
{
    long deadline = monotonic_ms() + SMOKE_HANDSHAKE_TIMEOUT_MS;

    while (state_table[peer_id] != expected) {
        if (monotonic_ms() >= deadline) {
            fail(step, "remote peer state timed out");
        }
        sleep_one_millisecond();
    }
}

static void three_peer_exchange(void *shared, uint32_t peer_id,
                                struct ivshmem_device *dev,
                                struct ivshmem_backend *backend)
{
    const volatile uint32_t *state_table = (const volatile uint32_t *)shared;
    volatile struct smoke_mailbox *own =
        section_mailbox(shared, peer_id,
                        SMOKE_THREE_OUTPUT_SECTION_STRIDE);
    volatile struct smoke_mailbox *coordinator =
        section_mailbox(shared, 0, SMOKE_THREE_OUTPUT_SECTION_STRIDE);
    volatile struct smoke_mailbox *target =
        section_mailbox(shared, 1, SMOKE_THREE_OUTPUT_SECTION_STRIDE);
    volatile struct smoke_mailbox *observer =
        section_mailbox(shared, 2, SMOKE_THREE_OUTPUT_SECTION_STRIDE);
    int wait_result;

    if (peer_id == 0) {
        fill_mailbox(own, 0x31);
        ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                            SMOKE_THREE_STATE_COORDINATOR);
        wait_remote_state_long(state_table, 1,
                               SMOKE_THREE_STATE_TARGET_READY,
                               "three-peer-target-ready");
        wait_remote_state_long(state_table, 2,
                               SMOKE_THREE_STATE_OBSERVER_ARMED,
                               "three-peer-observer-ready");
        __sync_synchronize();
        ivshmem_write_reg32(dev, IVSHMEM_REG_DOORBELL, 1u << 16);
        checkpoint("three-peer-target-doorbell");

        wait_result =
            ivshmem_backend_wait_event(backend, SMOKE_HANDSHAKE_TIMEOUT_MS);
        if (wait_result != 1) {
            fail("three-peer-reply",
                 wait_result == 0 ? "target reply timed out"
                                  : "target reply wait failed");
        }
        wait_remote_state_long(state_table, 1,
                               SMOKE_THREE_STATE_TARGET_REPLIED,
                               "three-peer-target-reply");
        wait_remote_state_long(state_table, 2,
                               SMOKE_THREE_STATE_OBSERVER_CLEAN,
                               "three-peer-observer-clean");
        validate_mailbox(coordinator, "three-peer-coordinator-payload");
        validate_mailbox(target, "three-peer-target-reply");
        validate_mailbox(observer, "three-peer-observer-payload");
        checkpoint("three-peer-visible-state-and-payload");
        return;
    }

    if (peer_id == 1) {
        ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                            SMOKE_THREE_STATE_TARGET_READY);
        wait_result =
            ivshmem_backend_wait_event(backend, SMOKE_HANDSHAKE_TIMEOUT_MS);
        if (wait_result != 1) {
            fail("three-peer-request",
                 wait_result == 0 ? "coordinator request timed out"
                                  : "coordinator request wait failed");
        }
        wait_remote_state_long(state_table, 0,
                               SMOKE_THREE_STATE_COORDINATOR,
                               "three-peer-coordinator-state");
        validate_mailbox(coordinator, "three-peer-coordinator-request");
        ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                            SMOKE_THREE_STATE_TARGET_RECEIVED);
        wait_remote_state_long(state_table, 2,
                               SMOKE_THREE_STATE_OBSERVER_CLEAN,
                               "three-peer-observer-clean");

        fill_mailbox(own, 0x52);
        ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                            SMOKE_THREE_STATE_TARGET_REPLIED);
        __sync_synchronize();
        ivshmem_write_reg32(dev, IVSHMEM_REG_DOORBELL, 0);
        checkpoint("three-peer-target-reply");
        return;
    }

    if (peer_id == 2) {
        long deadline;

        fill_mailbox(own, 0x73);
        __sync_synchronize();
        ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                            SMOKE_THREE_STATE_OBSERVER_ARMED);
        deadline = monotonic_ms() + SMOKE_HANDSHAKE_TIMEOUT_MS;
        while (state_table[1] != SMOKE_THREE_STATE_TARGET_RECEIVED) {
            if ((ivshmem_read_reg32(dev, IVSHMEM_REG_EVENT_STATUS) & 1) != 0) {
                fail("three-peer-isolation",
                     "peer 1 doorbell also reached peer 2");
            }
            if (monotonic_ms() >= deadline) {
                fail("three-peer-isolation",
                     "target did not receive the directed doorbell");
            }
            sleep_one_millisecond();
        }
        if ((ivshmem_read_reg32(dev, IVSHMEM_REG_EVENT_STATUS) & 1) != 0) {
            fail("three-peer-isolation", "peer 2 has a pending event");
        }
        ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                            SMOKE_THREE_STATE_OBSERVER_CLEAN);
        validate_mailbox(own, "three-peer-observer-payload");
        checkpoint("three-peer-observer-clean");
        return;
    }

    fail("three-peer-profile", "peer ID is outside the three-peer profile");
}

int main(int argc, char **argv)
{
    struct options options;
    struct ivshmem_device *dev = NULL;
    struct ivshmem_backend *backend = NULL;
    void *registers = NULL;
    void *shared = NULL;
    size_t register_size = 0;
    size_t shared_size = 0;
    uint32_t peer_id;
    uint32_t max_peers;
    uint32_t expected_max_peers;

    parse_options(argc, argv, &options);

    int result = ivshmem_find_device(options.bdf, &dev);
    if (result != IVSHMEM_OK) {
        fail_err("discover", result);
    }
    checkpoint("discover");

    result = ivshmem_enable_device(dev);
    if (result != IVSHMEM_OK) {
        fail_err("enable", result);
    }
    checkpoint("enable");

    result = ivshmem_map_bar(dev, IVSHMEM_BAR_REGISTERS, &registers,
                             &register_size);
    if (result != IVSHMEM_OK) {
        fail_err("map-registers", result);
    }
    if (register_size < IVSHMEM_REG_PAGE_SIZE) {
        fail("map-registers", "register BAR is smaller than one page");
    }
    result = ivshmem_map_bar(dev, IVSHMEM_BAR_SHARED, &shared, &shared_size);
    if (result != IVSHMEM_OK) {
        fail_err("map-shared", result);
    }
    checkpoint("map");

    peer_id = ivshmem_read_reg32(dev, IVSHMEM_REG_ID);
    max_peers = ivshmem_read_reg32(dev, IVSHMEM_REG_MAX_PEERS);
    expected_max_peers = options.three_peer ? 3u : 2u;
    if (max_peers != expected_max_peers) {
        char detail[128];

        snprintf(detail, sizeof(detail),
                 "max_peers reads %u (peer_id reads %u), expected %u",
                 (unsigned)max_peers, (unsigned)peer_id,
                 (unsigned)expected_max_peers);
        fail("profile", detail);
    }
    if (peer_id >= max_peers) {
        fail("profile", "peer ID is outside the advertised peer range");
    }
    if (shared_size <
        SMOKE_OUTPUT_SECTION_BASE +
            (size_t)max_peers *
                (options.three_peer ? SMOKE_THREE_OUTPUT_SECTION_STRIDE
                                    : SMOKE_OUTPUT_SECTION_STRIDE)) {
        fail("profile", "shared BAR is too small for the selected layout");
    }
    printf("ivshmem checkpoint profile peer_id=%u max_peers=%u "
           "shared_bytes=%zu\n",
           (unsigned)peer_id, (unsigned)max_peers, shared_size);

    if (ivshmem_shared_memory(dev, &shared_size) != shared) {
        fail("map-shared", "shared-memory mapping is not cached");
    }
    exchange_payload(shared, peer_id,
                     options.three_peer ? SMOKE_THREE_OUTPUT_SECTION_STRIDE
                                        : SMOKE_OUTPUT_SECTION_STRIDE);

    /* The BAR0 State write must surface in the shared state table: this
     * peer's entry sits at BAR2 offset `peer_id * 4` inside the first page
     * (F4 layout). The three-peer protocol publishes role-specific state
     * after all peers have opened their event backend. */
    const volatile uint32_t *state_table = (const volatile uint32_t *)shared;
    if (!options.three_peer) {
        ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                            SMOKE_HANDSHAKE_STATE_SELF);
        if (state_table[peer_id] != SMOKE_HANDSHAKE_STATE_SELF) {
            fail("state",
                 "BAR0 state write did not surface in the state table");
        }
        checkpoint("state");
    }

    /* F5/F6 evidence: the state table is read-only for every peer and each
     * peer may write only its own output section. The writes below must
     * fault into the guest and leave the backing bytes untouched. */
    deny_write_tests(shared, peer_id, max_peers,
                     options.three_peer ? SMOKE_THREE_OUTPUT_SECTION_STRIDE
                                        : SMOKE_OUTPUT_SECTION_STRIDE);

    result = ivshmem_backend_open(dev, options.backend, &backend);
    if (result != IVSHMEM_OK) {
        fail_err("backend", result);
    }

    if (options.three_peer) {
        three_peer_exchange(shared, peer_id, dev, backend);
    } else if (options.cross_peer) {
        if (peer_id == 0) {
            /* Initiator: self-doorbell coverage first, then the exchange.
             * Its own events are consumed by the self tests, so the reply
             * wait below cannot observe a stale event. */
            self_doorbell_tests(dev, backend, peer_id);
            cross_peer_exchange(shared, peer_id, dev, backend);
        } else {
            /* Responder: the exchange consumes the request event before any
             * self doorbell can merge with it. */
            cross_peer_exchange(shared, peer_id, dev, backend);
            self_doorbell_tests(dev, backend, peer_id);
        }
    } else {
        self_doorbell_tests(dev, backend, peer_id);
    }

    /* Vector 1 is outside the current profile: the doorbell is a no-op and
     * no event may arrive. */
    ivshmem_write_reg32(dev, IVSHMEM_REG_DOORBELL, (peer_id << 16) | 1u);
    int wait_result = ivshmem_backend_wait_event(backend, 200);
    if (wait_result != 0) {
        fail("doorbell", "an unsupported vector produced an event");
    }
    checkpoint("doorbell-unsupported-vector");

    ivshmem_backend_close(backend);
    ivshmem_device_close(dev);

    if (options.three_peer) {
        if (peer_id == 0) {
            printf("ivshmem %s three-peer pass\n", selected_backend);
        } else {
            printf("ivshmem %s three-peer relay pass\n", selected_backend);
        }
        fflush(stdout);
        return 0;
    }
    if (options.cross_peer && peer_id != 0) {
        /* The responder's marker is distinct: the case succeeds only when
         * the initiator observes the completed round trip. */
        printf("ivshmem %s relay pass\n", selected_backend);
        fflush(stdout);
        return 0;
    }

    printf("ivshmem %s pass\n", selected_backend);
    fflush(stdout);
    return 0;
}
