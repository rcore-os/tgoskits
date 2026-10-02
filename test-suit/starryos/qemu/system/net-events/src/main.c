/*
 * Checks the net:* events end to end:
 *
 *   1. every event is discoverable and its format declares the documented
 *      fields;
 *   2. enabling them makes real records readable from the trace buffer, with
 *      internally consistent values, for the events that traffic drives;
 *   3. disabling them stops new records while the same traffic continues.
 *
 * The events carry no network semantics beyond the runtime's own facts, so the
 * traffic below only has to produce queue rounds and protocol activity, not
 * traffic patterns.
 */

#define _GNU_SOURCE
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#define EVENT_BASE "/sys/kernel/debug/tracing/events/net"
#define TRACE_FILE "/sys/kernel/debug/tracing/trace"
#define PATH_LEN 160
#define MAX_TRACE_BYTES (256 * 1024)
#define MAX_FIELDS 6

/* The QEMU command line this suite runs under uses user-mode networking, whose
 * gateway answers ARP and IP, so a datagram sent to it always leaves the
 * interface.  Loopback traffic would not: it is injected into the protocol
 * receive path without touching a physical queue. */
#define GATEWAY_ADDR "10.0.2.2"
#define GATEWAY_PORT 9
#define TRAFFIC_FRAMES 16
/* A datagram is refused while the interface still has no routable address
 * (DHCP can take seconds), so the retry window is generous rather than tuned:
 * the case may fail for a broken event, not for a slow network. */
#define TRAFFIC_ATTEMPTS 50
#define TRAFFIC_RETRY_US 100000
#define RECORD_WAIT_ATTEMPTS 20
#define RECORD_WAIT_US 50000
/* The same budget the record wait above is allowed, so the negative check
 * cannot pass merely by reading the buffer before the rounds finished. */
#define DISABLED_WAIT_US (RECORD_WAIT_ATTEMPTS * RECORD_WAIT_US)
/* Time for a round that already passed the gate check to reach the buffer
 * before the buffer is cleared, so the negative check does not read a record
 * that was produced while the event was still enabled. */
#define DISABLE_SETTLE_US 50000

/* How a record of one event is checked once real traffic has produced one. */
enum record_shape {
    /* This case does not assert this event's semantics: the outcome needs the
     * device to race or to be busy, which this case's traffic does not
     * guarantee.  Only discovery, format and the disabled leg are checked
     * here; the semantics are covered by the ax-net unit tests. */
    SHAPE_UNDRIVEN,
    /* A queue poll round: budget, work units and outcome. */
    SHAPE_POLL,
    /* A frame length on top of the shared queue identity fields. */
    SHAPE_FRAME,
    /* A protocol executor yield: owner CPU, reason and pending work. */
    SHAPE_YIELD,
};

struct event_spec {
    const char *name;
    int field_count;
    const char *fields[MAX_FIELDS];
    enum record_shape shape;
};

static const struct event_spec EVENTS[] = {
    { "queue_poll_round",
      6,
      { "discovery_order", "group_id", "owner_cpu", "budget", "work_units", "outcome" },
      SHAPE_POLL },
    /* A rearm race and a busy device are reachable in the driver but not
     * guaranteed under this case's traffic. */
    { "queue_rearm",
      4,
      { "discovery_order", "group_id", "owner_cpu", "outcome" },
      SHAPE_UNDRIVEN },
    { "queue_backpressure",
      5,
      { "discovery_order", "group_id", "owner_cpu", "stage", "reason" },
      SHAPE_UNDRIVEN },
    { "tx_submit",
      4,
      { "discovery_order", "group_id", "owner_cpu", "frame_len" },
      SHAPE_FRAME },
    { "rx_publish",
      4,
      { "discovery_order", "group_id", "owner_cpu", "frame_len" },
      SHAPE_FRAME },
    { "proto_yield",
      3,
      { "owner_cpu", "reason", "work_pending" },
      SHAPE_YIELD },
};

#define EVENT_COUNT (sizeof(EVENTS) / sizeof(EVENTS[0]))

static int failures;

static void fail(const char *message)
{
    printf("NET_EVENTS_FAIL: %s\n", message);
    failures++;
}

static long read_file(const char *path, char *buffer, size_t capacity)
{
    FILE *file = fopen(path, "r");
    if (file == NULL) {
        return -1;
    }
    size_t total = 0;
    size_t read;
    while (total + 1 < capacity &&
           (read = fread(buffer + total, 1, capacity - 1 - total, file)) > 0) {
        total += read;
    }
    fclose(file);
    buffer[total] = '\0';
    return (long)total;
}

static int write_file(const char *path, const char *text)
{
    FILE *file = fopen(path, "w");
    if (file == NULL) {
        return -1;
    }
    int result = fputs(text, file) >= 0 ? 0 : -1;
    if (fclose(file) != 0) {
        result = -1;
    }
    return result;
}

static void event_path(char *path, size_t capacity, const char *name, const char *attribute)
{
    snprintf(path, capacity, EVENT_BASE "/%s/%s", name, attribute);
}

/* A rendered record reads `queue_poll_round(discovery_order=0 ... outcome=0)`. */
static int count_records(const char *text, const char *name)
{
    char marker[64];
    snprintf(marker, sizeof(marker), "%s(", name);
    int count = 0;
    const char *cursor = text;
    while ((cursor = strstr(cursor, marker)) != NULL) {
        count++;
        cursor += strlen(marker);
    }
    return count;
}

/* Sends datagrams off the box, which drives real transmit and receive rounds. */
static int send_traffic(void)
{
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) {
        return -1;
    }
    struct sockaddr_in peer;
    memset(&peer, 0, sizeof(peer));
    peer.sin_family = AF_INET;
    peer.sin_port = htons(GATEWAY_PORT);
    if (inet_pton(AF_INET, GATEWAY_ADDR, &peer.sin_addr) != 1) {
        close(fd);
        return -1;
    }

    unsigned char payload[32];
    memset(payload, 0x5a, sizeof(payload));
    int sent = 0;
    /* Retried until at least one datagram is accepted, so a not-yet-routable
     * interface delays the check instead of failing it. */
    for (int attempt = 0; attempt < TRAFFIC_ATTEMPTS && sent == 0; attempt++) {
        for (int index = 0; index < TRAFFIC_FRAMES; index++) {
            payload[0] = (unsigned char)index;
            ssize_t result = sendto(fd, payload, sizeof(payload), 0, (struct sockaddr *)&peer,
                                    sizeof(peer));
            if (result == (ssize_t)sizeof(payload)) {
                sent++;
            }
        }
        if (sent == 0) {
            usleep(TRAFFIC_RETRY_US);
        }
    }
    close(fd);
    return sent;
}

/* Verifies that one event is present with the documented format. */
static void check_discovery(const struct event_spec *event)
{
    char path[PATH_LEN];
    static char buffer[8192];

    event_path(path, sizeof(path), event->name, "id");
    char *id_end = NULL;
    if (read_file(path, buffer, sizeof(buffer)) <= 0) {
        printf("NET_EVENTS_FAIL: net:%s has no readable id\n", event->name);
        failures++;
    } else if (strtol(buffer, &id_end, 10) < 0 || id_end == buffer) {
        printf("NET_EVENTS_FAIL: net:%s reports a non-numeric id\n", event->name);
        failures++;
    }
    event_path(path, sizeof(path), event->name, "format");
    if (read_file(path, buffer, sizeof(buffer)) <= 0) {
        printf("NET_EVENTS_FAIL: net:%s has no readable format\n", event->name);
        failures++;
        return;
    }
    for (int index = 0; index < event->field_count; index++) {
        if (strstr(buffer, event->fields[index]) == NULL) {
            printf("NET_EVENTS_FAIL: net:%s format lacks field %s\n", event->name,
                   event->fields[index]);
            failures++;
        }
    }
    if (strstr(buffer, event->name) == NULL) {
        printf("NET_EVENTS_FAIL: net:%s format does not describe the event\n", event->name);
        failures++;
    }
}

static int set_all_events(const char *value)
{
    int failures_seen = 0;
    for (size_t index = 0; index < EVENT_COUNT; index++) {
        char path[PATH_LEN];
        event_path(path, sizeof(path), EVENTS[index].name, "enable");
        if (write_file(path, value) != 0) {
            printf("NET_EVENTS_FAIL: net:%s could not be written %s\n", EVENTS[index].name,
                   value);
            failures_seen++;
        }
    }
    return failures_seen;
}

static void check_owner_cpu(const char *name, unsigned owner_cpu)
{
    long online_cpus = sysconf(_SC_NPROCESSORS_ONLN);
    if (online_cpus > 0 && owner_cpu >= (unsigned long)online_cpus) {
        printf("NET_EVENTS_FAIL: net:%s reports an owner CPU outside the online set\n", name);
        failures++;
    }
}

/* Checks the first record of an event whose payload is a single length. */
static void check_frame_record(const struct event_spec *event, const char *trace)
{
    char marker[64];
    snprintf(marker, sizeof(marker), "%s(", event->name);
    const char *marker_at = strstr(trace, marker);
    if (marker_at == NULL) {
        printf("NET_EVENTS_FAIL: no net:%s record\n", event->name);
        failures++;
        return;
    }
    const char *record = marker_at + strlen(marker);
    unsigned discovery_order = 0;
    unsigned group_id = 0;
    unsigned owner_cpu = 0;
    unsigned frame_len = 0;
    int fields = sscanf(record, "discovery_order=%u group_id=%u owner_cpu=%u frame_len=%u",
                        &discovery_order, &group_id, &owner_cpu, &frame_len);
    if (fields != 4) {
        printf("NET_EVENTS_FAIL: net:%s record does not carry the documented fields\n",
               event->name);
        failures++;
        return;
    }
    if (frame_len == 0) {
        printf("NET_EVENTS_FAIL: net:%s record reports a zero-length frame\n", event->name);
        failures++;
    }
    check_owner_cpu(event->name, owner_cpu);
    printf("NET_EVENTS_RECORD %s discovery_order=%u group_id=%u owner_cpu=%u frame_len=%u\n",
           event->name, discovery_order, group_id, owner_cpu, frame_len);
}

/* Parses the first poll-round record and checks its internal consistency. */
static void check_poll_record(const char *trace)
{
    static const char marker[] = "queue_poll_round(";
    const char *marker_at = strstr(trace, marker);
    if (marker_at == NULL) {
        fail("trace buffer holds no queue_poll_round record");
        return;
    }
    const char *record = marker_at + sizeof(marker) - 1;
    unsigned discovery_order = 0;
    unsigned group_id = 0;
    unsigned owner_cpu = 0;
    unsigned budget = 0;
    unsigned work_units = 0;
    unsigned outcome = 0;
    int fields = sscanf(record, "discovery_order=%u group_id=%u owner_cpu=%u budget=%u "
                                "work_units=%u outcome=%u",
                        &discovery_order, &group_id, &owner_cpu, &budget, &work_units, &outcome);
    if (fields != 6) {
        fail("queue_poll_round record does not carry the documented fields");
        return;
    }
    if (outcome > 3) {
        fail("queue_poll_round record reports an unknown outcome code");
    }
    if (work_units > budget) {
        fail("queue_poll_round record reports more work than its budget");
    }
    check_owner_cpu("queue_poll_round", owner_cpu);
    printf("NET_EVENTS_RECORD queue_poll_round discovery_order=%u group_id=%u owner_cpu=%u "
           "budget=%u work_units=%u outcome=%u\n",
           discovery_order, group_id, owner_cpu, budget, work_units, outcome);
}

/* Parses the first protocol yield record and checks its internal consistency. */
static void check_yield_record(const char *trace)
{
    static const char marker[] = "proto_yield(";
    const char *record = strstr(trace, marker);
    if (record == NULL) {
        fail("trace buffer holds no proto_yield record");
        return;
    }
    record += sizeof(marker) - 1;
    unsigned owner_cpu = 0;
    unsigned reason = 0;
    unsigned work_pending = 0;
    int fields =
        sscanf(record, "owner_cpu=%u reason=%u work_pending=%u", &owner_cpu, &reason, &work_pending);
    if (fields != 3) {
        fail("proto_yield record does not carry the documented fields");
        return;
    }
    if (reason > 2) {
        fail("proto_yield record reports an unknown yield reason");
    }
    if (work_pending > 1) {
        fail("proto_yield record reports an unknown pending-work code");
    }
    check_owner_cpu("proto_yield", owner_cpu);
    printf("NET_EVENTS_RECORD proto_yield owner_cpu=%u reason=%u work_pending=%u\n", owner_cpu,
           reason, work_pending);
}

/* Whether every event this case expects traffic to drive has a record. */
static int all_traffic_events_recorded(const char *trace)
{
    for (size_t index = 0; index < EVENT_COUNT; index++) {
        const struct event_spec *event = &EVENTS[index];
        if (event->shape == SHAPE_UNDRIVEN) {
            continue;
        }
        if (count_records(trace, event->name) == 0) {
            return 0;
        }
    }
    return 1;
}

/* Reads the trace buffer, waiting briefly for records to be published.
 *
 * Each traffic-driven event is produced by a different boundary (a queue
 * round, a driver acceptance, a protocol-side publish, a protocol executor
 * yield), so the wait covers all of them rather than only the first one to
 * appear; a bounded wait keeps a broken event a failure instead of a race. */
static long read_trace_waiting_for_all(char *buffer, size_t capacity)
{
    long length = -1;
    for (int attempt = 0; attempt < RECORD_WAIT_ATTEMPTS; attempt++) {
        length = read_file(TRACE_FILE, buffer, capacity);
        if (length < 0) {
            return -1;
        }
        if (all_traffic_events_recorded(buffer)) {
            break;
        }
        usleep(RECORD_WAIT_US);
    }
    return length;
}

int main(void)
{
    static char buffer[MAX_TRACE_BYTES];

    for (size_t index = 0; index < EVENT_COUNT; index++) {
        check_discovery(&EVENTS[index]);
    }

    if (write_file(TRACE_FILE, "\n") != 0) {
        fail("trace buffer could not be cleared");
    }
    if (set_all_events("1") != 0) {
        fail("the net:* events could not be enabled");
        printf("NET_EVENTS_FAILED\n");
        return 1;
    }

    int sent = send_traffic();
    if (sent <= 0) {
        fail("no traffic could be sent to drive queue rounds");
    }

    long trace_length = read_trace_waiting_for_all(buffer, sizeof(buffer));
    if (trace_length < 0) {
        fail("trace buffer is not readable");
    } else {
        int records = count_records(buffer, "queue_poll_round");
        if (records == 0) {
            fail("enabled events recorded no queue poll round");
        } else {
            printf("NET_EVENTS records=%d\n", records);
            check_poll_record(buffer);
            for (size_t index = 0; index < EVENT_COUNT; index++) {
                const struct event_spec *event = &EVENTS[index];
                if (event->shape == SHAPE_UNDRIVEN || event->shape == SHAPE_POLL) {
                    continue;
                }
                if (count_records(buffer, event->name) == 0) {
                    printf("NET_EVENTS_FAIL: no net:%s record under real traffic\n", event->name);
                    failures++;
                    continue;
                }
                if (event->shape == SHAPE_FRAME) {
                    check_frame_record(event, buffer);
                } else {
                    check_yield_record(buffer);
                }
            }
        }
    }

    if (set_all_events("0") != 0) {
        fail("the net:* events could not be disabled");
    }
    /* A round that already passed the gate check may still be on its way to
     * the buffer; let it land before the buffer is cleared, so the negative
     * check cannot read a record the enabled events produced. */
    usleep(DISABLE_SETTLE_US);
    if (write_file(TRACE_FILE, "\n") != 0) {
        fail("trace buffer could not be cleared after disabling");
    }
    sent = send_traffic();
    if (sent <= 0) {
        fail("no traffic could be sent while the events were disabled");
    }
    usleep(DISABLED_WAIT_US);
    trace_length = read_file(TRACE_FILE, buffer, sizeof(buffer));
    if (trace_length < 0) {
        fail("trace buffer is not readable after disabling");
    } else {
        for (size_t index = 0; index < EVENT_COUNT; index++) {
            if (count_records(buffer, EVENTS[index].name) != 0) {
                printf("NET_EVENTS_FAIL: a disabled net:%s still recorded rounds\n",
                       EVENTS[index].name);
                failures++;
            }
        }
    }

    fflush(stdout);
    if (failures != 0) {
        puts("NET_EVENTS_FAILED");
        return 1;
    }
    puts("NET_EVENTS_PASSED");
    return 0;
}
