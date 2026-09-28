#define _GNU_SOURCE

#include <errno.h>
#include <limits.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

static void usage(const char *program)
{
    fprintf(stderr,
            "Usage: %s check <expected-cpus>\n"
            "       %s run <cpu-count> <program> [args...]\n",
            program, program);
}

static int parse_cpu_count(const char *text, int *cpu_count)
{
    char *end = NULL;
    long value;

    errno = 0;
    value = strtol(text, &end, 10);
    if (errno != 0 || end == text || *end != '\0' || value <= 0 ||
        value > CPU_SETSIZE || value > INT_MAX) {
        return -1;
    }

    *cpu_count = (int)value;
    return 0;
}

static int raw_get_affinity(cpu_set_t *mask)
{
    CPU_ZERO(mask);
    if (syscall(SYS_sched_getaffinity, 0, sizeof(*mask), mask) < 0) {
        fprintf(stderr, "sched_getaffinity failed: %s\n", strerror(errno));
        return -1;
    }
    return 0;
}

static int raw_set_affinity(const cpu_set_t *mask)
{
    if (syscall(SYS_sched_setaffinity, 0, sizeof(*mask), mask) < 0) {
        fprintf(stderr, "sched_setaffinity failed: %s\n", strerror(errno));
        return -1;
    }
    return 0;
}

static unsigned long long low_mask(const cpu_set_t *mask)
{
    unsigned long long value = 0;
    int limit = CPU_SETSIZE < 64 ? CPU_SETSIZE : 64;

    for (int cpu = 0; cpu < limit; cpu++) {
        if (CPU_ISSET(cpu, mask)) {
            value |= 1ULL << cpu;
        }
    }
    return value;
}

static int check_topology(int expected_cpus)
{
    cpu_set_t allowed;
    long online = sysconf(_SC_NPROCESSORS_ONLN);

    if (online < 0) {
        fprintf(stderr, "sysconf(_SC_NPROCESSORS_ONLN) failed: %s\n",
                strerror(errno));
        return 1;
    }
    if (raw_get_affinity(&allowed) != 0) {
        return 1;
    }

    int allowed_count = CPU_COUNT(&allowed);
    printf("LTP_HACKBENCH_TOPOLOGY online=%ld allowed=%d mask=0x%llx\n",
           online, allowed_count, low_mask(&allowed));
    if (online != expected_cpus || allowed_count != expected_cpus) {
        fprintf(stderr,
                "expected %d online and allowed CPUs, got online=%ld "
                "allowed=%d\n",
                expected_cpus, online, allowed_count);
        return 1;
    }
    return 0;
}

static int run_with_affinity(int cpu_count, char **command)
{
    cpu_set_t allowed;
    cpu_set_t selected;
    cpu_set_t observed;

    if (raw_get_affinity(&allowed) != 0) {
        return 1;
    }
    if (CPU_COUNT(&allowed) < cpu_count) {
        fprintf(stderr, "requested %d CPUs but only %d are allowed\n", cpu_count,
                CPU_COUNT(&allowed));
        return 1;
    }

    CPU_ZERO(&selected);
    int remaining = cpu_count;
    for (int cpu = 0; cpu < CPU_SETSIZE && remaining > 0; cpu++) {
        if (CPU_ISSET(cpu, &allowed)) {
            CPU_SET(cpu, &selected);
            remaining--;
        }
    }

    if (raw_set_affinity(&selected) != 0 || raw_get_affinity(&observed) != 0) {
        return 1;
    }
    if (!CPU_EQUAL(&selected, &observed)) {
        fprintf(stderr,
                "affinity round-trip mismatch: requested=0x%llx observed=0x%llx\n",
                low_mask(&selected), low_mask(&observed));
        return 1;
    }

    printf("LTP_HACKBENCH_AFFINITY cpus=%d mask=0x%llx\n", cpu_count,
           low_mask(&observed));
    fflush(stdout);
    execv(command[0], command);
    fprintf(stderr, "execv(%s) failed: %s\n", command[0], strerror(errno));
    return 1;
}

int main(int argc, char **argv)
{
    int cpu_count;

    if (argc == 3 && strcmp(argv[1], "check") == 0) {
        if (parse_cpu_count(argv[2], &cpu_count) != 0) {
            fprintf(stderr, "invalid expected CPU count: %s\n", argv[2]);
            return 2;
        }
        return check_topology(cpu_count);
    }

    if (argc >= 4 && strcmp(argv[1], "run") == 0) {
        if (parse_cpu_count(argv[2], &cpu_count) != 0) {
            fprintf(stderr, "invalid CPU count: %s\n", argv[2]);
            return 2;
        }
        if (argv[3][0] != '/') {
            fprintf(stderr, "program path must be absolute: %s\n", argv[3]);
            return 2;
        }
        return run_with_affinity(cpu_count, &argv[3]);
    }

    usage(argv[0]);
    return 2;
}
