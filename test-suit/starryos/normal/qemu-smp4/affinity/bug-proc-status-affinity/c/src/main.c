#define _GNU_SOURCE

#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static void expect(int condition, const char *message) {
    if (!condition) {
        fputs(message, stderr);
        fputc('\n', stderr);
        abort();
    }
}

static void trim_newline(char *text) {
    size_t len = strlen(text);
    if (len > 0 && text[len - 1] == '\n') {
        text[len - 1] = '\0';
    }
}

static void read_affinity_lines(char *allowed, size_t allowed_size, char *allowed_list,
                                size_t allowed_list_size) {
    FILE *fp = fopen("/proc/self/status", "r");
    expect(fp != NULL, "failed to open /proc/self/status");

    char line[256];
    int found_allowed = 0;
    int found_allowed_list = 0;

    while (fgets(line, sizeof(line), fp) != NULL) {
        if (strncmp(line, "Cpus_allowed:\t", 14) == 0) {
            snprintf(allowed, allowed_size, "%s", line + 14);
            trim_newline(allowed);
            found_allowed = 1;
        } else if (strncmp(line, "Cpus_allowed_list:\t", 19) == 0) {
            snprintf(allowed_list, allowed_list_size, "%s", line + 19);
            trim_newline(allowed_list);
            found_allowed_list = 1;
        }
    }

    fclose(fp);
    expect(found_allowed, "missing Cpus_allowed in /proc/self/status");
    expect(found_allowed_list, "missing Cpus_allowed_list in /proc/self/status");
}

static void read_first_line(const char *path, char *buf, size_t size) {
    FILE *fp = fopen(path, "r");
    expect(fp != NULL, path);
    expect(fgets(buf, size, fp) != NULL, path);
    fclose(fp);
    trim_newline(buf);
}

static int cpu_range_count(const char *range) {
    int start = -1;
    int end = -1;
    if (sscanf(range, "%d-%d", &start, &end) == 2) {
        return end >= start ? end - start + 1 : 0;
    }
    if (sscanf(range, "%d", &start) == 1) {
        return 1;
    }
    return 0;
}

static int cpuinfo_processor_count(void) {
    FILE *fp = fopen("/proc/cpuinfo", "r");
    expect(fp != NULL, "failed to open /proc/cpuinfo");

    char line[256];
    int count = 0;
    while (fgets(line, sizeof(line), fp) != NULL) {
        if (strncmp(line, "processor", 9) == 0) {
            count++;
        }
    }

    fclose(fp);
    return count;
}

int main(void) {
    long cpu_num = sysconf(_SC_NPROCESSORS_ONLN);
    expect(cpu_num >= 4, "expected at least 4 online CPUs");

    cpu_set_t default_mask;
    CPU_ZERO(&default_mask);
    expect(sched_getaffinity(0, sizeof(default_mask), &default_mask) == 0,
           "default sched_getaffinity failed");
    expect(CPU_COUNT(&default_mask) >= 4, "default affinity exposes fewer than 4 CPUs");

    char allowed[64];
    char allowed_list[64];
    read_affinity_lines(allowed, sizeof(allowed), allowed_list, sizeof(allowed_list));
    expect(strcmp(allowed, "0000000f") == 0, "initial Cpus_allowed should expose CPUs 0-3");
    expect(strcmp(allowed_list, "0-3") == 0, "initial Cpus_allowed_list should expose CPUs 0-3");

    expect(cpuinfo_processor_count() >= 4, "/proc/cpuinfo exposes fewer than 4 CPUs");

    char sysfs_range[64];
    read_first_line("/sys/devices/system/cpu/online", sysfs_range, sizeof(sysfs_range));
    expect(cpu_range_count(sysfs_range) >= 4, "sysfs online exposes fewer than 4 CPUs");
    read_first_line("/sys/devices/system/cpu/possible", sysfs_range, sizeof(sysfs_range));
    expect(cpu_range_count(sysfs_range) >= 4, "sysfs possible exposes fewer than 4 CPUs");
    read_first_line("/sys/devices/system/cpu/present", sysfs_range, sizeof(sysfs_range));
    expect(cpu_range_count(sysfs_range) >= 4, "sysfs present exposes fewer than 4 CPUs");

    cpu_set_t mask;
    CPU_ZERO(&mask);
    CPU_SET(1, &mask);
    expect(sched_setaffinity(0, sizeof(mask), &mask) == 0, "sched_setaffinity failed");

    cpu_set_t current_mask;
    CPU_ZERO(&current_mask);
    expect(sched_getaffinity(0, sizeof(current_mask), &current_mask) == 0,
           "sched_getaffinity failed");
    expect(CPU_ISSET(1, &current_mask), "CPU1 not present in current affinity");
    expect(CPU_COUNT(&current_mask) == 1, "current affinity should contain exactly one CPU");

    read_affinity_lines(allowed, sizeof(allowed), allowed_list, sizeof(allowed_list));

    expect(strcmp(allowed, "00000002") == 0, "unexpected Cpus_allowed");
    expect(strcmp(allowed_list, "1") == 0, "unexpected Cpus_allowed_list");

    puts("TEST PASSED");
    return 0;
}
