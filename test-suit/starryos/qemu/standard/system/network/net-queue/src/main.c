/*
 * Checks the debugfs queue snapshot: /sys/kernel/debug/net_queue must report
 * one complete record per poll group, and every record must name an interface
 * that /proc/net/dev also reports.
 */

#define _GNU_SOURCE
#include <ctype.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define MAX_INTERFACES 16
#define NAME_LEN 32
#define CPU_FIELD_LEN 16
#define LINE_LEN 512

/* Mirrors one /sys/kernel/debug/net_queue record.  Only the identity columns
 * asserted below are inspected; the counters are parsed so that the record has
 * to carry every column, not to check their values (those are covered by the
 * ax-net unit tests). */
struct queue_group {
    unsigned long long discovery_order;
    unsigned long long group_id;
    char interface[NAME_LEN];
    unsigned long long owner_cpu;
    unsigned long long irq;
    unsigned long long schedule;
    unsigned long long missed;
    unsigned long long poll_batches;
    unsigned long long budget_exhaustion;
    unsigned long long spurious;
    unsigned long long probe_deferred;
    unsigned long long rearm_race;
    char last_irq_cpu[CPU_FIELD_LEN];
    char last_poll_cpu[CPU_FIELD_LEN];
    unsigned long long irq_to_poll_remote_wake;
    unsigned long long rx_drops;
};

static int failures;

static void report_failure(const char *message, const char *line)
{
    printf("NET_QUEUE_FAIL: %s: %s", message, line);
    failures++;
}

/* Collect the non-loopback interface names of /proc/net/dev. */
static int read_interfaces(char names[][NAME_LEN], int capacity)
{
    FILE *file = fopen("/proc/net/dev", "r");
    if (file == NULL) {
        return -1;
    }

    char line[LINE_LEN];
    int count = 0;
    while (fgets(line, sizeof(line), file) != NULL) {
        char *colon = strchr(line, ':');
        if (colon == NULL) {
            continue;
        }
        char *name = line;
        while (*name != '\0' && isspace((unsigned char)*name)) {
            name++;
        }
        size_t length = (size_t)(colon - name);
        if (length == 0 || length >= NAME_LEN) {
            continue;
        }
        if (length == 2 && strncmp(name, "lo", 2) == 0) {
            continue;
        }
        if (count == capacity) {
            /* Dropping names here would turn "more interfaces than expected"
             * into a misleading "interface is not in /proc/net/dev" later. */
            printf("NET_QUEUE_FAIL: more than %d non-loopback interfaces\n", capacity);
            failures++;
            break;
        }
        snprintf(names[count], NAME_LEN, "%.*s", (int)length, name);
        count++;
    }

    fclose(file);
    return count;
}

static int is_known_interface(char names[][NAME_LEN], int count, const char *name)
{
    for (int index = 0; index < count; index++) {
        if (strcmp(names[index], name) == 0) {
            return 1;
        }
    }
    return 0;
}

static int is_cpu_field(const char *field)
{
    if (strcmp(field, "-") == 0) {
        return 1;
    }
    if (field[0] == '\0') {
        return 0;
    }
    for (const char *cursor = field; *cursor != '\0'; cursor++) {
        if (!isdigit((unsigned char)*cursor)) {
            return 0;
        }
    }
    return 1;
}

/* Parses one rendered record into exactly 16 fields: the trailing conversion
 * rejects an extra column, a short record leaves it unmatched. */
static int parse_group(const char *line, struct queue_group *group)
{
    char extra[2];
    int fields = sscanf(
        line,
        "%llu %llu %31s %llu %llu %llu %llu %llu %llu %llu %llu %llu %15s %15s %llu %llu %1s",
        &group->discovery_order,
        &group->group_id,
        group->interface,
        &group->owner_cpu,
        &group->irq,
        &group->schedule,
        &group->missed,
        &group->poll_batches,
        &group->budget_exhaustion,
        &group->spurious,
        &group->probe_deferred,
        &group->rearm_race,
        group->last_irq_cpu,
        group->last_poll_cpu,
        &group->irq_to_poll_remote_wake,
        &group->rx_drops,
        extra);
    return fields == 16;
}

int main(void)
{
    char interfaces[MAX_INTERFACES][NAME_LEN];
    int interface_count = read_interfaces(interfaces, MAX_INTERFACES);
    if (interface_count < 0) {
        perror("open /proc/net/dev");
        puts("NET_QUEUE_FAILED");
        return 1;
    }

    FILE *file = fopen("/sys/kernel/debug/net_queue", "r");
    if (file == NULL) {
        perror("open /sys/kernel/debug/net_queue");
        puts("NET_QUEUE_FAILED");
        return 1;
    }

    long online_cpus = sysconf(_SC_NPROCESSORS_ONLN);
    if (online_cpus <= 0) {
        /* Say so instead of silently skipping the owner CPU bound below. */
        puts("NET_QUEUE_NOTE: online CPU count unavailable, owner CPU bound not checked");
    }

    char line[LINE_LEN];
    int header_lines = 0;
    int groups = 0;
    while (fgets(line, sizeof(line), file) != NULL) {
        if (line[0] == '#') {
            header_lines++;
            continue;
        }
        if (line[0] == '\n' || line[0] == '\0') {
            continue;
        }

        struct queue_group group;
        if (!parse_group(line, &group)) {
            report_failure("queue record does not hold exactly 16 fields", line);
            continue;
        }
        if (!is_known_interface(interfaces, interface_count, group.interface)) {
            report_failure("queue group names an interface /proc/net/dev does not report",
                           line);
        }
        if (!is_cpu_field(group.last_irq_cpu) || !is_cpu_field(group.last_poll_cpu)) {
            report_failure("queue group has a malformed CPU field", line);
        }
        if (online_cpus > 0 && group.owner_cpu >= (unsigned long long)online_cpus) {
            report_failure("queue group reports an owner CPU outside the online set", line);
        }
        groups++;
    }
    fclose(file);

    if (header_lines == 0) {
        puts("NET_QUEUE_FAIL: missing column header");
        failures++;
    }
    /* Every system-test QEMU configuration attaches a NIC, so a boot without
     * one means the network runtime regressed instead of this check not
     * applying. */
    if (interface_count == 0) {
        puts("NET_QUEUE_FAIL: /proc/net/dev reports no non-loopback interface");
        failures++;
    }
    if (groups == 0) {
        puts("NET_QUEUE_FAIL: no queue group reported for a live interface");
        failures++;
    }

    printf("NET_QUEUE groups=%d interfaces=%d\n", groups, interface_count);
    fflush(stdout);
    if (failures != 0) {
        puts("NET_QUEUE_FAILED");
        return 1;
    }
    puts("NET_QUEUE_PASSED");
    return 0;
}
