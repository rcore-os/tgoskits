/*
 * bug-proc-stat-monotonic: /proc/stat CPU counters must not go backwards
 * after a short-lived CPU-bound child exits.
 *
 * Old behavior: /proc/stat recomputed aggregate user/system time from the
 * currently-live task table.  When a rustc/build-script-like child consumed
 * CPU and exited, its task could disappear from that table and the aggregate
 * "cpu" counters dropped, breaking CPU-utilization monitors used by the
 * StarryOS self-build experiments.
 */
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int read_proc_stat_total(uint64_t *total)
{
    FILE *fp = fopen("/proc/stat", "r");
    if (fp == NULL) {
        printf("fopen /proc/stat failed: errno=%d (%s)\n", errno, strerror(errno));
        return -1;
    }

    char line[256];
    if (fgets(line, sizeof(line), fp) == NULL) {
        printf("fgets /proc/stat failed\n");
        fclose(fp);
        return -1;
    }
    fclose(fp);

    char tag[8];
    unsigned long long user = 0;
    unsigned long long nice = 0;
    unsigned long long system = 0;
    unsigned long long idle = 0;
    int n = sscanf(line, "%7s %llu %llu %llu %llu", tag, &user, &nice, &system, &idle);
    if (n != 5 || strcmp(tag, "cpu") != 0) {
        printf("unexpected /proc/stat first line: %s\n", line);
        return -1;
    }

    (void)idle;
    *total = (uint64_t)(user + nice + system);
    return 0;
}

static uint64_t monotonic_ms(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return 0;
    }
    return (uint64_t)ts.tv_sec * 1000 + (uint64_t)ts.tv_nsec / 1000000;
}

static void burn_cpu_for_ms(uint64_t duration_ms)
{
    uint64_t end = monotonic_ms() + duration_ms;
    volatile uint64_t x = 0x12345678u;
    while (monotonic_ms() < end) {
        for (int i = 0; i < 4096; i++) {
            x = x * 1103515245u + 12345u;
        }
    }
    _exit((int)(x & 1u));
}

int main(void)
{
    printf("=== bug-proc-stat-monotonic ===\n");

    uint64_t before = 0;
    if (read_proc_stat_total(&before) != 0) {
        return 1;
    }

    pid_t child = fork();
    if (child < 0) {
        printf("fork failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }
    if (child == 0) {
        burn_cpu_for_ms(350);
    }

    uint64_t peak = before;
    int status = 0;
    for (;;) {
        uint64_t now = 0;
        if (read_proc_stat_total(&now) != 0) {
            return 1;
        }
        if (now > peak) {
            peak = now;
        }

        pid_t ret = waitpid(child, &status, WNOHANG);
        if (ret == child) {
            break;
        }
        if (ret < 0) {
            printf("waitpid failed: errno=%d (%s)\n", errno, strerror(errno));
            return 1;
        }

        struct timespec pause = {
            .tv_sec = 0,
            .tv_nsec = 20 * 1000 * 1000,
        };
        nanosleep(&pause, NULL);
    }

    if (!WIFEXITED(status)) {
        printf("child did not exit normally: status=%d\n", status);
        return 1;
    }

    uint64_t after = 0;
    if (read_proc_stat_total(&after) != 0) {
        return 1;
    }

    printf("proc_stat_cpu_total before=%llu peak=%llu after=%llu\n",
           (unsigned long long)before,
           (unsigned long long)peak,
           (unsigned long long)after);

    if (after < peak) {
        printf("FAIL: /proc/stat cpu total went backwards after child exit\n");
        return 1;
    }

    printf("PASS: /proc/stat cpu total is monotonic across child exit\n");
    return 0;
}
