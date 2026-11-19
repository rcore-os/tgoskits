#define _DEFAULT_SOURCE
#define _POSIX_C_SOURCE 200809L

/*
 * Three-peer ivshmem-pci end-to-end suite.
 *
 * The permanent topology is Linux, ArceOS, and a portable peer intended for
 * Zephyr. Until a Zephyr demo exists, a second Linux guest implements the
 * portable peer role with the polling backend. The Linux primary runs this
 * binary twice: polling first, then interrupt after the init script loads the
 * UIO modules. This keeps the portable role independent from Linux UIO and
 * makes the later Zephyr replacement local to that peer.
 */
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "ivshmem.h"

#define SUITE_MAX_PEERS 3u
#define SUITE_LINUX_PEER 0u
#define SUITE_ARCEOS_PEER 1u
#define SUITE_PORTABLE_PEER 2u

#define SUITE_OUTPUT_SECTION_BASE 0x1000u
#define SUITE_OUTPUT_SECTION_STRIDE 0x5000u
#define SUITE_PAYLOAD_SIZE 0x100u
#define SUITE_MAILBOX_MAGIC 0x49565355u
#define SUITE_PROTOCOL_VERSION 1u
#define SUITE_HANDSHAKE_TIMEOUT_MS 30000

#define SUITE_PHASE_LINUX_POLLING 1u
#define SUITE_PHASE_ARCEOS_TO_LINUX_IRQ 2u
#define SUITE_SEQUENCE_LINUX_POLLING 1u
#define SUITE_SEQUENCE_ARCEOS_TO_LINUX_IRQ 2u

#define SUITE_STATE_LINUX_POLLING_BOOTSTRAP 0x31000000u
#define SUITE_STATE_LINUX_POLLING_REQUEST 0x31000001u
#define SUITE_STATE_LINUX_POLLING_DONE 0x31000002u
#define SUITE_STATE_LINUX_IRQ_BOOTSTRAP 0x31000100u
#define SUITE_STATE_LINUX_IRQ_READY 0x31000101u
#define SUITE_STATE_LINUX_IRQ_RECEIVED 0x31000102u
#define SUITE_STATE_LINUX_IRQ_REPLIED 0x31000103u
#define SUITE_STATE_DONE 0x31000104u

#define SUITE_STATE_ARCEOS_OBSERVER_ARMED 0x32000001u
#define SUITE_STATE_ARCEOS_OBSERVER_CLEAN 0x32000002u
#define SUITE_STATE_ARCEOS_REQUEST 0x32000003u
#define SUITE_STATE_ARCEOS_DONE 0x32000004u

#define SUITE_STATE_PORTABLE_BOOTSTRAP 0x33000000u
#define SUITE_STATE_PORTABLE_READY 0x33000001u
#define SUITE_STATE_PORTABLE_RECEIVED 0x33000002u
#define SUITE_STATE_PORTABLE_REPLIED 0x33000003u
#define SUITE_STATE_PORTABLE_OBSERVER_ARMED 0x33000004u
#define SUITE_STATE_PORTABLE_OBSERVER_CLEAN 0x33000005u

static const char LINUX_TO_PORTABLE_REQUEST[] =
    "Hello portable peer, this is Linux primary using polling.";
static const char PORTABLE_TO_LINUX_REPLY[] =
    "Hello Linux primary, portable peer received the polling message.";
static const char ARCEOS_TO_LINUX_REQUEST[] =
    "Hello Linux primary, ArceOS requests an MSI-X round trip.";
static const char LINUX_TO_ARCEOS_REPLY[] =
    "Hello ArceOS, Linux primary received the MSI-X message.";

struct suite_mailbox {
    uint32_t magic;
    uint32_t protocol_version;
    uint32_t phase;
    uint32_t sequence;
    uint32_t source_peer;
    uint32_t target_peer;
    uint32_t checksum;
    uint32_t payload_size;
    uint8_t payload[SUITE_PAYLOAD_SIZE];
};

enum suite_role {
    SUITE_ROLE_LINUX_POLLING,
    SUITE_ROLE_LINUX_INTERRUPT,
    SUITE_ROLE_PORTABLE_PEER,
};

struct options {
    const char *bdf;
    enum suite_role role;
    int role_selected;
};

static const char *selected_role = "unknown";
static const char *selected_phase = "bootstrap";
static uint32_t selected_peer = UINT32_MAX;

static void fail(const char *step, const char *detail)
{
    if (selected_peer == UINT32_MAX) {
        printf("IVSHMEM_PCI_SUITE_FAILED peer=unknown role=%s phase=%s "
               "step=%s detail=%s\n",
               selected_role, selected_phase, step, detail);
    } else {
        printf("IVSHMEM_PCI_SUITE_FAILED peer=%u role=%s phase=%s "
               "step=%s detail=%s\n",
               (unsigned)selected_peer, selected_role, selected_phase, step,
               detail);
    }
    fflush(stdout);
    exit(1);
}

static void fail_err(const char *step, int err)
{
    fail(step, ivshmem_strerror(err));
}

static void checkpoint(const char *step)
{
    printf("IVSHMEM_PCI_SUITE_CHECKPOINT peer=%u role=%s phase=%s step=%s\n",
           (unsigned)selected_peer, selected_role, selected_phase, step);
    fflush(stdout);
}

static void usage(const char *program)
{
    fprintf(stderr,
            "usage: %s --suite-role "
            "linux-polling|linux-interrupt|portable-peer [--bdf <BDF>]\n",
            program);
}

static void parse_options(int argc, char **argv, struct options *options)
{
    int index;

    memset(options, 0, sizeof(*options));
    for (index = 1; index < argc; index++) {
        if (strcmp(argv[index], "--suite-role") == 0 && index + 1 < argc) {
            index++;
            options->role_selected = 1;
            if (strcmp(argv[index], "linux-polling") == 0) {
                options->role = SUITE_ROLE_LINUX_POLLING;
                selected_role = "linux-polling";
            } else if (strcmp(argv[index], "linux-interrupt") == 0) {
                options->role = SUITE_ROLE_LINUX_INTERRUPT;
                selected_role = "linux-interrupt";
            } else if (strcmp(argv[index], "portable-peer") == 0) {
                options->role = SUITE_ROLE_PORTABLE_PEER;
                selected_role = "portable-peer-linux";
            } else {
                fail("args", "unknown suite role");
            }
        } else if (strcmp(argv[index], "--bdf") == 0 && index + 1 < argc) {
            options->bdf = argv[++index];
        } else {
            usage(argv[0]);
            fail("args", "unrecognized command line");
        }
    }
    if (!options->role_selected) {
        usage(argv[0]);
        fail("args", "--suite-role is required");
    }
}

static uint32_t expected_peer_for_role(enum suite_role role)
{
    return role == SUITE_ROLE_PORTABLE_PEER ? SUITE_PORTABLE_PEER
                                            : SUITE_LINUX_PEER;
}

static enum ivshmem_backend_kind backend_for_role(enum suite_role role)
{
    return role == SUITE_ROLE_LINUX_INTERRUPT
               ? IVSHMEM_BACKEND_INTERRUPT
               : IVSHMEM_BACKEND_POLLING;
}

static uint32_t bootstrap_state_for_role(enum suite_role role)
{
    switch (role) {
    case SUITE_ROLE_LINUX_POLLING:
        return SUITE_STATE_LINUX_POLLING_BOOTSTRAP;
    case SUITE_ROLE_LINUX_INTERRUPT:
        return SUITE_STATE_LINUX_IRQ_BOOTSTRAP;
    case SUITE_ROLE_PORTABLE_PEER:
        return SUITE_STATE_PORTABLE_BOOTSTRAP;
    }
    fail("role", "invalid suite role");
    return 0;
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

static uint32_t payload_checksum(const volatile uint8_t *payload, size_t size)
{
    uint32_t checksum = 0x49565348u;
    size_t index;

    for (index = 0; index < size; index++) {
        checksum = checksum * 31u + payload[index];
    }
    return checksum;
}

static volatile struct suite_mailbox *section_mailbox(void *shared,
                                                       uint32_t peer_id)
{
    size_t offset = SUITE_OUTPUT_SECTION_BASE +
                    (size_t)peer_id * SUITE_OUTPUT_SECTION_STRIDE;

    return (volatile struct suite_mailbox *)((volatile uint8_t *)shared +
                                              offset);
}

static void fill_mailbox(volatile struct suite_mailbox *mailbox,
                         uint32_t phase, uint32_t sequence,
                         uint32_t source_peer, uint32_t target_peer,
                         const char *message)
{
    size_t message_size = strlen(message);
    size_t index;

    if (message_size == 0 || message_size > SUITE_PAYLOAD_SIZE) {
        fail("message", "outgoing message length is invalid");
    }
    mailbox->magic = 0;
    for (index = 0; index < SUITE_PAYLOAD_SIZE; index++) {
        mailbox->payload[index] = 0;
    }
    for (index = 0; index < message_size; index++) {
        mailbox->payload[index] = (uint8_t)message[index];
    }
    mailbox->protocol_version = SUITE_PROTOCOL_VERSION;
    mailbox->phase = phase;
    mailbox->sequence = sequence;
    mailbox->source_peer = source_peer;
    mailbox->target_peer = target_peer;
    mailbox->payload_size = (uint32_t)message_size;
    mailbox->checksum = payload_checksum(mailbox->payload, message_size);
    __sync_synchronize();
    mailbox->magic = SUITE_MAILBOX_MAGIC;
}

static void validate_mailbox(const volatile struct suite_mailbox *mailbox,
                             uint32_t phase, uint32_t sequence,
                             uint32_t source_peer, uint32_t target_peer,
                             const char *expected_message, const char *step)
{
    size_t expected_size = strlen(expected_message);
    size_t index;
    if (mailbox->magic != SUITE_MAILBOX_MAGIC) {
        fail(step, "mailbox magic mismatch");
    }
    if (mailbox->protocol_version != SUITE_PROTOCOL_VERSION) {
        fail(step, "mailbox protocol version mismatch");
    }
    if (mailbox->phase != phase || mailbox->sequence != sequence) {
        fail(step, "mailbox phase or sequence mismatch");
    }
    if (mailbox->source_peer != source_peer ||
        mailbox->target_peer != target_peer) {
        fail(step, "mailbox route mismatch");
    }
    if (mailbox->payload_size != expected_size) {
        fail(step, "mailbox payload size mismatch");
    }
    if (mailbox->checksum != payload_checksum(mailbox->payload,
                                               mailbox->payload_size)) {
        fail(step, "mailbox payload checksum mismatch");
    }
    for (index = 0; index < expected_size; index++) {
        if (mailbox->payload[index] != (uint8_t)expected_message[index]) {
            fail(step, "mailbox message content mismatch");
        }
    }
}

static void log_mailbox(const char *action,
                        const volatile struct suite_mailbox *mailbox,
                        const char *path)
{
    char message[SUITE_PAYLOAD_SIZE + 1];
    size_t message_size = mailbox->payload_size;
    size_t index;

    if (message_size > SUITE_PAYLOAD_SIZE) {
        fail("message-log", "mailbox payload size exceeds the log buffer");
    }
    for (index = 0; index < message_size; index++) {
        message[index] = (char)mailbox->payload[index];
    }
    message[message_size] = '\0';
    printf("IVSHMEM_PCI_SUITE_MESSAGE action=%s phase=%u sequence=%u "
           "source=%u target=%u path=%s content=\"%s\"\n",
           action, (unsigned)mailbox->phase, (unsigned)mailbox->sequence,
           (unsigned)mailbox->source_peer, (unsigned)mailbox->target_peer,
           path, message);
    fflush(stdout);
}

static void wait_remote_state(const volatile uint32_t *state_table,
                              uint32_t peer_id, uint32_t expected,
                              const char *step)
{
    long deadline = monotonic_ms() + SUITE_HANDSHAKE_TIMEOUT_MS;

    /* Peer states advance monotonically; an asynchronous peer can publish
     * the next checkpoint before this reader samples the previous one. */
    while (state_table[peer_id] < expected) {
        if (monotonic_ms() >= deadline) {
            char detail[128];

            snprintf(detail, sizeof(detail),
                     "peer %u state is 0x%08x, expected 0x%08x",
                     (unsigned)peer_id, (unsigned)state_table[peer_id],
                     (unsigned)expected);
            fail(step, detail);
        }
        sleep_one_millisecond();
    }
}

static void ensure_no_event_until_state(
    struct ivshmem_device *dev, const volatile uint32_t *state_table,
    uint32_t peer_id, uint32_t expected, const char *step)
{
    long deadline = monotonic_ms() + SUITE_HANDSHAKE_TIMEOUT_MS;

    while (state_table[peer_id] < expected) {
        if ((ivshmem_read_reg32(dev, IVSHMEM_REG_EVENT_STATUS) & 1u) != 0) {
            fail(step, "a directed doorbell reached the non-target peer");
        }
        if (monotonic_ms() >= deadline) {
            fail(step, "target peer did not finish the directed phase");
        }
        sleep_one_millisecond();
    }
    if ((ivshmem_read_reg32(dev, IVSHMEM_REG_EVENT_STATUS) & 1u) != 0) {
        fail(step, "the non-target peer has a pending event");
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
        fail(step, "a denied write modified the shared bytes");
    }
}

static void run_permission_checks(void *shared, uint32_t peer_id)
{
    volatile uint32_t *state_entry = (volatile uint32_t *)shared + peer_id;
    uint32_t remote_peer = (peer_id + 1u) % SUITE_MAX_PEERS;
    volatile uint32_t *remote_output =
        (volatile uint32_t *)((volatile uint8_t *)shared +
                              SUITE_OUTPUT_SECTION_BASE +
                              (size_t)remote_peer *
                                  SUITE_OUTPUT_SECTION_STRIDE);

    install_deny_signal_handlers();
    expect_denied_write(state_entry, "deny-write-state");
    checkpoint("deny-write-state");
    expect_denied_write(remote_output, "deny-write-output");
    checkpoint("deny-write-output");
}

static void run_local_event_checks(struct ivshmem_device *dev,
                                   struct ivshmem_backend *backend,
                                   uint32_t peer_id)
{
    int wait_result;

    ivshmem_write_reg32(dev, IVSHMEM_REG_DOORBELL, peer_id << 16);
    wait_result = ivshmem_backend_wait_event(backend, 5000);
    if (wait_result != 1) {
        fail("doorbell-first", wait_result == 0 ? "event timed out"
                                                : "event wait failed");
    }
    if ((ivshmem_read_reg32(dev, IVSHMEM_REG_EVENT_STATUS) & 1u) != 0) {
        fail("doorbell-first", "Event Status was not cleared");
    }
    checkpoint("doorbell-first");

    ivshmem_write_reg32(dev, IVSHMEM_REG_DOORBELL, peer_id << 16);
    wait_result = ivshmem_backend_wait_event(backend, 5000);
    if (wait_result != 1) {
        fail("doorbell-second", wait_result == 0 ? "event timed out"
                                                 : "event wait failed");
    }
    checkpoint("doorbell-second");

    ivshmem_write_reg32(dev, IVSHMEM_REG_DOORBELL,
                        (peer_id << 16) | 1u);
    wait_result = ivshmem_backend_wait_event(backend, 200);
    if (wait_result != 0) {
        fail("doorbell-unsupported-vector",
             "an unsupported vector produced an event");
    }
    checkpoint("doorbell-unsupported-vector");
}

static void run_linux_polling_phase(struct ivshmem_device *dev, void *shared,
                                    struct ivshmem_backend *backend)
{
    const volatile uint32_t *state_table =
        (const volatile uint32_t *)shared;
    volatile struct suite_mailbox *own =
        section_mailbox(shared, SUITE_LINUX_PEER);
    volatile struct suite_mailbox *portable =
        section_mailbox(shared, SUITE_PORTABLE_PEER);
    int wait_result;

    selected_phase = "linux-polling";
    wait_remote_state(state_table, SUITE_PORTABLE_PEER,
                      SUITE_STATE_PORTABLE_READY, "portable-ready");
    wait_remote_state(state_table, SUITE_ARCEOS_PEER,
                      SUITE_STATE_ARCEOS_OBSERVER_ARMED,
                      "arceos-observer-ready");

    fill_mailbox(own, SUITE_PHASE_LINUX_POLLING,
                 SUITE_SEQUENCE_LINUX_POLLING, SUITE_LINUX_PEER,
                 SUITE_PORTABLE_PEER, LINUX_TO_PORTABLE_REQUEST);
    log_mailbox("tx", own, "doorbell");
    ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                        SUITE_STATE_LINUX_POLLING_REQUEST);
    __sync_synchronize();
    ivshmem_write_reg32(dev, IVSHMEM_REG_DOORBELL,
                        SUITE_PORTABLE_PEER << 16);
    checkpoint("portable-request");

    wait_result =
        ivshmem_backend_wait_event(backend, SUITE_HANDSHAKE_TIMEOUT_MS);
    if (wait_result != 1) {
        fail("portable-reply", wait_result == 0 ? "reply timed out"
                                                : "event wait failed");
    }
    wait_remote_state(state_table, SUITE_PORTABLE_PEER,
                      SUITE_STATE_PORTABLE_REPLIED, "portable-replied");
    wait_remote_state(state_table, SUITE_ARCEOS_PEER,
                      SUITE_STATE_ARCEOS_OBSERVER_CLEAN,
                      "arceos-observer-clean");
    validate_mailbox(portable, SUITE_PHASE_LINUX_POLLING,
                     SUITE_SEQUENCE_LINUX_POLLING, SUITE_PORTABLE_PEER,
                     SUITE_LINUX_PEER, PORTABLE_TO_LINUX_REPLY,
                     "portable-reply");
    log_mailbox("rx", portable, "polling");
    ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                        SUITE_STATE_LINUX_POLLING_DONE);
    checkpoint("phase-complete");
}

static void run_linux_interrupt_phase(struct ivshmem_device *dev,
                                      void *shared,
                                      struct ivshmem_backend *backend)
{
    const volatile uint32_t *state_table =
        (const volatile uint32_t *)shared;
    volatile struct suite_mailbox *own =
        section_mailbox(shared, SUITE_LINUX_PEER);
    volatile struct suite_mailbox *arceos =
        section_mailbox(shared, SUITE_ARCEOS_PEER);
    int wait_result;

    selected_phase = "linux-interrupt";
    wait_remote_state(state_table, SUITE_ARCEOS_PEER,
                      SUITE_STATE_ARCEOS_OBSERVER_CLEAN,
                      "arceos-observer-clean");
    wait_remote_state(state_table, SUITE_PORTABLE_PEER,
                      SUITE_STATE_PORTABLE_OBSERVER_ARMED,
                      "portable-observer-ready");
    ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                        SUITE_STATE_LINUX_IRQ_READY);
    checkpoint("irq-ready");

    wait_result =
        ivshmem_backend_wait_event(backend, SUITE_HANDSHAKE_TIMEOUT_MS);
    if (wait_result != 1) {
        fail("arceos-request", wait_result == 0 ? "request timed out"
                                                : "event wait failed");
    }
    wait_remote_state(state_table, SUITE_ARCEOS_PEER,
                      SUITE_STATE_ARCEOS_REQUEST, "arceos-request-state");
    validate_mailbox(arceos, SUITE_PHASE_ARCEOS_TO_LINUX_IRQ,
                     SUITE_SEQUENCE_ARCEOS_TO_LINUX_IRQ,
                     SUITE_ARCEOS_PEER, SUITE_LINUX_PEER,
                     ARCEOS_TO_LINUX_REQUEST, "arceos-request");
    log_mailbox("rx", arceos, "interrupt");
    ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                        SUITE_STATE_LINUX_IRQ_RECEIVED);

    fill_mailbox(own, SUITE_PHASE_ARCEOS_TO_LINUX_IRQ,
                 SUITE_SEQUENCE_ARCEOS_TO_LINUX_IRQ, SUITE_LINUX_PEER,
                 SUITE_ARCEOS_PEER, LINUX_TO_ARCEOS_REPLY);
    log_mailbox("tx", own, "doorbell");
    ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                        SUITE_STATE_LINUX_IRQ_REPLIED);
    __sync_synchronize();
    ivshmem_write_reg32(dev, IVSHMEM_REG_DOORBELL,
                        SUITE_ARCEOS_PEER << 16);
    checkpoint("arceos-reply");

    wait_remote_state(state_table, SUITE_ARCEOS_PEER,
                      SUITE_STATE_ARCEOS_DONE, "arceos-done");
    wait_remote_state(state_table, SUITE_PORTABLE_PEER,
                      SUITE_STATE_PORTABLE_OBSERVER_CLEAN,
                      "portable-observer-clean");
    ivshmem_write_reg32(dev, IVSHMEM_REG_STATE, SUITE_STATE_DONE);
    checkpoint("suite-complete");
}

static void run_portable_peer_phase(struct ivshmem_device *dev, void *shared,
                                    struct ivshmem_backend *backend)
{
    const volatile uint32_t *state_table =
        (const volatile uint32_t *)shared;
    volatile struct suite_mailbox *own =
        section_mailbox(shared, SUITE_PORTABLE_PEER);
    volatile struct suite_mailbox *linux =
        section_mailbox(shared, SUITE_LINUX_PEER);
    int wait_result;

    selected_phase = "portable-responder";
    ivshmem_write_reg32(dev, IVSHMEM_REG_STATE, SUITE_STATE_PORTABLE_READY);
    checkpoint("ready");
    wait_result =
        ivshmem_backend_wait_event(backend, SUITE_HANDSHAKE_TIMEOUT_MS);
    if (wait_result != 1) {
        fail("linux-request", wait_result == 0 ? "request timed out"
                                               : "event wait failed");
    }
    wait_remote_state(state_table, SUITE_LINUX_PEER,
                      SUITE_STATE_LINUX_POLLING_REQUEST,
                      "linux-request-state");
    validate_mailbox(linux, SUITE_PHASE_LINUX_POLLING,
                     SUITE_SEQUENCE_LINUX_POLLING, SUITE_LINUX_PEER,
                     SUITE_PORTABLE_PEER, LINUX_TO_PORTABLE_REQUEST,
                     "linux-request");
    log_mailbox("rx", linux, "polling");
    ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                        SUITE_STATE_PORTABLE_RECEIVED);

    fill_mailbox(own, SUITE_PHASE_LINUX_POLLING,
                 SUITE_SEQUENCE_LINUX_POLLING, SUITE_PORTABLE_PEER,
                 SUITE_LINUX_PEER, PORTABLE_TO_LINUX_REPLY);
    log_mailbox("tx", own, "doorbell");
    ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                        SUITE_STATE_PORTABLE_REPLIED);
    __sync_synchronize();
    ivshmem_write_reg32(dev, IVSHMEM_REG_DOORBELL,
                        SUITE_LINUX_PEER << 16);
    checkpoint("linux-reply");

    wait_remote_state(state_table, SUITE_ARCEOS_PEER,
                      SUITE_STATE_ARCEOS_OBSERVER_CLEAN,
                      "arceos-observer-clean");
    selected_phase = "portable-observer";
    ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                        SUITE_STATE_PORTABLE_OBSERVER_ARMED);
    checkpoint("observer-ready");
    ensure_no_event_until_state(dev, state_table, SUITE_ARCEOS_PEER,
                                SUITE_STATE_ARCEOS_DONE,
                                "arceos-to-linux-isolation");
    ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                        SUITE_STATE_PORTABLE_OBSERVER_CLEAN);
    checkpoint("observer-clean");
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
    const volatile uint32_t *state_table;
    uint32_t peer_id;
    uint32_t max_peers;
    uint32_t expected_peer;
    int result;

    parse_options(argc, argv, &options);
    result = ivshmem_find_device(options.bdf, &dev);
    if (result != IVSHMEM_OK) {
        fail_err("discover", result);
    }
    result = ivshmem_enable_device(dev);
    if (result != IVSHMEM_OK) {
        fail_err("enable", result);
    }
    result = ivshmem_map_bar(dev, IVSHMEM_BAR_REGISTERS, &registers,
                             &register_size);
    if (result != IVSHMEM_OK || register_size < IVSHMEM_REG_PAGE_SIZE) {
        fail("map-registers", "BAR0 is unavailable or too small");
    }
    result = ivshmem_map_bar(dev, IVSHMEM_BAR_SHARED, &shared, &shared_size);
    if (result != IVSHMEM_OK) {
        fail_err("map-shared", result);
    }

    peer_id = ivshmem_read_reg32(dev, IVSHMEM_REG_ID);
    max_peers = ivshmem_read_reg32(dev, IVSHMEM_REG_MAX_PEERS);
    selected_peer = peer_id;
    expected_peer = expected_peer_for_role(options.role);
    if (max_peers != SUITE_MAX_PEERS || peer_id != expected_peer) {
        char detail[128];

        snprintf(detail, sizeof(detail),
                 "profile peer_id=%u max_peers=%u, expected peer_id=%u "
                 "max_peers=%u",
                 (unsigned)peer_id, (unsigned)max_peers,
                 (unsigned)expected_peer, (unsigned)SUITE_MAX_PEERS);
        fail("profile", detail);
    }
    if (shared_size < SUITE_OUTPUT_SECTION_BASE +
                          SUITE_MAX_PEERS * SUITE_OUTPUT_SECTION_STRIDE) {
        fail("profile", "BAR2 is too small for the three-peer layout");
    }
    checkpoint("profile");

    state_table = (const volatile uint32_t *)shared;
    ivshmem_write_reg32(dev, IVSHMEM_REG_STATE,
                        bootstrap_state_for_role(options.role));
    if (state_table[peer_id] != bootstrap_state_for_role(options.role)) {
        fail("state", "BAR0 State did not update the BAR2 state table");
    }
    checkpoint("state");
    run_permission_checks(shared, peer_id);

    result = ivshmem_backend_open(dev, backend_for_role(options.role),
                                  &backend);
    if (result != IVSHMEM_OK) {
        fail_err("backend", result);
    }
    run_local_event_checks(dev, backend, peer_id);

    switch (options.role) {
    case SUITE_ROLE_LINUX_POLLING:
        run_linux_polling_phase(dev, shared, backend);
        break;
    case SUITE_ROLE_LINUX_INTERRUPT:
        run_linux_interrupt_phase(dev, shared, backend);
        break;
    case SUITE_ROLE_PORTABLE_PEER:
        run_portable_peer_phase(dev, shared, backend);
        break;
    }

    ivshmem_backend_close(backend);
    ivshmem_device_close(dev);
    if (options.role == SUITE_ROLE_LINUX_INTERRUPT) {
        printf("IVSHMEM_PCI_SUITE_PASSED\n");
    } else {
        printf("IVSHMEM_PCI_SUITE_ROLE_PASSED peer=%u role=%s\n",
               (unsigned)peer_id, selected_role);
    }
    fflush(stdout);
    return 0;
}
