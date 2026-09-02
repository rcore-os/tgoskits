/*
 * A task that has completed its final context switch must not retain itself
 * through an Arc stored in the abandoned switch frame.  Such a reference pins
 * the TaskInner and its 256 KiB kernel stack forever even after waitpid() has
 * reaped the process.
 *
 * Keep each live wave small, but create enough fully reaped tasks that the old
 * self-reference loses roughly 192 MiB.  A generous 96 MiB budget separates
 * that deterministic leak from page-cache and asynchronous-reclaimer noise on
 * every 512 MiB Starry QEMU target.
 */

#define _GNU_SOURCE

#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

enum {
    BATCHES = 12,
    CHILDREN_PER_BATCH = 64,
    TOTAL_CHILDREN = BATCHES * CHILDREN_PER_BATCH,
    RECLAIM_POLLS = 40,
    RECLAIM_POLL_US = 50000,
};

#define RECLAIM_BUDGET_KB (96L * 1024L)

static long read_memfree_kb(void)
{
    FILE *file = fopen("/proc/meminfo", "r");
    if (file == NULL)
        return -1;

    char line[128];
    long value = -1;
    while (fgets(line, sizeof(line), file) != NULL) {
        if (sscanf(line, "MemFree: %ld kB", &value) == 1)
            break;
    }
    fclose(file);
    return value;
}

static long wait_for_reclaim(long baseline_kb)
{
    long observed = read_memfree_kb();
    for (int poll = 0;
         poll < RECLAIM_POLLS && baseline_kb > 0 && observed > 0 &&
         baseline_kb - observed >= RECLAIM_BUDGET_KB;
         poll++) {
        usleep(RECLAIM_POLL_US);
        observed = read_memfree_kb();
    }
    return observed;
}

int main(void)
{
    printf("=== test-task-stack-reclaim ===\n");
    long free_before = read_memfree_kb();
    printf("INFO: MemFree before %d fork/reap operations: %ld kB\n",
           TOTAL_CHILDREN, free_before);

    int created = 0;
    int failures = 0;
    for (int batch = 0; batch < BATCHES; batch++) {
        pid_t children[CHILDREN_PER_BATCH];
        int batch_children = 0;

        for (int index = 0; index < CHILDREN_PER_BATCH; index++) {
            pid_t child = fork();
            if (child < 0) {
                printf("FAIL: fork %d failed: %s (errno=%d)\n", created,
                       strerror(errno), errno);
                failures++;
                break;
            }
            if (child == 0)
                _exit(0);
            children[batch_children++] = child;
            created++;
        }

        for (int index = 0; index < batch_children; index++) {
            int status = 0;
            if (waitpid(children[index], &status, 0) != children[index] ||
                !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
                printf("FAIL: child %d was not reaped cleanly\n",
                       (int)children[index]);
                failures++;
            }
        }
        if (batch_children != CHILDREN_PER_BATCH)
            break;

        /* Give the scheduler GC and MM reclaimer an opportunity to process
         * every task whose waitpid transaction has completed. */
        usleep(RECLAIM_POLL_US);
    }

    long free_after = wait_for_reclaim(free_before);
    long delta = free_before > 0 && free_after > 0
                     ? free_before - free_after
                     : -1;
    printf("INFO: created=%d MemFree after=%ld kB delta=%ld kB\n", created,
           free_after, delta);

    if (created != TOTAL_CHILDREN) {
        printf("FAIL: only %d/%d tasks were created\n", created,
               TOTAL_CHILDREN);
        failures++;
    }
    if (free_before <= 0 || free_after <= 0) {
        printf("FAIL: /proc/meminfo did not expose MemFree\n");
        failures++;
    } else if (delta >= RECLAIM_BUDGET_KB) {
        printf("FAIL: reaped task stacks retained %ld kB (budget %ld kB)\n",
               delta, RECLAIM_BUDGET_KB);
        failures++;
    }

    if (failures != 0) {
        printf("TEST FAILED: %d failure(s)\n", failures);
        return 1;
    }
    printf("PASS: all reaped task stacks became reclaimable\n");
    printf("TEST PASSED\n");
    return 0;
}
