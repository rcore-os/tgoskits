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

int main(void) {
    long cpu_num = sysconf(_SC_NPROCESSORS_ONLN);
    expect(cpu_num >= 2, "expected at least 2 online CPUs");

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

    char allowed[64];
    char allowed_list[64];
    read_affinity_lines(allowed, sizeof(allowed), allowed_list, sizeof(allowed_list));

    expect(strcmp(allowed, "00000002") == 0, "unexpected Cpus_allowed");
    expect(strcmp(allowed_list, "1") == 0, "unexpected Cpus_allowed_list");

    puts("TEST PASSED");
    return 0;
}
