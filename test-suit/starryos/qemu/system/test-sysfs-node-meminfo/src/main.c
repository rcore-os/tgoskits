/*
 * The only memory node and /proc/meminfo describe the same RAM, so their free
 * figures agree up to the allocation churn between two reads. Kernel task
 * stacks count on both sides: 64 parked threads each hold one, which a view
 * that skipped them would report as free. Recorded on Linux 6.6.
 */
#define _GNU_SOURCE
#include "test_framework.h"
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#define THREADS 64
#define SLACK_KB 1024

static int gate[2];

static void *park(void *arg)
{
    char byte;
    if (read(gate[0], &byte, 1) != 1)
        return arg;
    return NULL;
}

static long free_kb(const char *path)
{
    FILE *f = fopen(path, "r");
    if (!f)
        return -1;
    char line[256];
    long kb = -1;
    while (kb < 0 && fgets(line, sizeof line, f)) {
        char *at = strstr(line, "MemFree:");
        if (at && sscanf(at + strlen("MemFree:"), "%ld", &kb) != 1)
            kb = -1;
    }
    fclose(f);
    return kb;
}

int main(void)
{
    TEST_START("node0 meminfo agrees with /proc/meminfo");
    CHECK_RET(pipe(gate), 0, "pipe");

    pthread_t threads[THREADS];
    int started = 0;
    while (started < THREADS && pthread_create(&threads[started], NULL, park, NULL) == 0)
        started++;
    CHECK(started == THREADS, "start 64 parked threads");

    for (int round = 0; round < 3; round++) {
        long before = free_kb("/proc/meminfo");
        long node = free_kb("/sys/devices/system/node/node0/meminfo");
        long after = free_kb("/proc/meminfo");
        printf("  round %d: /proc/meminfo %ld..%ld kB, node0 %ld kB\n", round, before, after, node);
        CHECK(before > 0 && node > 0 && after > 0, "both files report MemFree");
        long lo = (before < after ? before : after) - SLACK_KB;
        long hi = (before > after ? before : after) + SLACK_KB;
        CHECK(node >= lo && node <= hi, "node0 MemFree lies within /proc/meminfo MemFree");
    }

    char release[THREADS];
    memset(release, 1, sizeof release);
    CHECK(write(gate[1], release, started) == started, "release the parked threads");
    for (int i = 0; i < started; i++)
        pthread_join(threads[i], NULL);
    TEST_DONE();
}
