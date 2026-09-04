/*
 * bug-clone-affinity-inherit: a child created by fork() and a thread created by
 * CLONE_THREAD must inherit the parent/creator CPU affinity mask, matching
 * Linux (p->cpus_ptr is copied on clone). Previously new tasks kept the default
 * full mask, so a taskset-pinned process spawned workers that escaped the pin.
 */
#define _GNU_SOURCE

#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

/* Pin everything to CPU 0 so the expected inherited mask is exactly {0}. */
static int pin_to_cpu0(void)
{
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(0, &set);
    return sched_setaffinity(0, sizeof(set), &set);
}

/* Returns 0 if the calling task's affinity mask is exactly {CPU 0}. */
static int affinity_is_cpu0_only(const char *who)
{
    cpu_set_t set;
    CPU_ZERO(&set);
    if (sched_getaffinity(0, sizeof(set), &set) != 0) {
        printf("FAIL: %s sched_getaffinity: %s\n", who, strerror(errno));
        return -1;
    }
    if (!CPU_ISSET(0, &set) || CPU_COUNT(&set) != 1) {
        printf("FAIL: %s inherited mask has %d CPUs (want exactly {0})\n", who,
               CPU_COUNT(&set));
        return -1;
    }
    return 0;
}

static void *thread_main(void *arg)
{
    (void)arg;
    /* Non-zero return signals inheritance failure to the joiner. */
    return (void *)(intptr_t)(affinity_is_cpu0_only("thread") == 0 ? 0 : 1);
}

int main(void)
{
    printf("=== bug-clone-affinity-inherit ===\n");
    printf("Expected: fork() child and CLONE_THREAD thread both inherit the\n");
    printf("          parent affinity mask {CPU 0}.\n\n");

    if (pin_to_cpu0() != 0) {
        printf("FAIL: sched_setaffinity(self, {0}): %s\n", strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }
    if (affinity_is_cpu0_only("parent") != 0) {
        printf("TEST FAILED\n");
        return 1;
    }
    printf("PASS: parent pinned to {CPU 0}\n");

    /* fork() child must inherit {CPU 0}. */
    pid_t pid = fork();
    if (pid < 0) {
        printf("FAIL: fork: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }
    if (pid == 0) {
        _exit(affinity_is_cpu0_only("fork-child") == 0 ? 0 : 1);
    }
    int status = 0;
    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0) {
        printf("FAIL: fork child did not inherit {CPU 0}\n");
        printf("TEST FAILED\n");
        return 1;
    }
    printf("PASS: fork() child inherited {CPU 0}\n");

    /* CLONE_THREAD thread must inherit {CPU 0}. */
    pthread_t tid;
    if (pthread_create(&tid, NULL, thread_main, NULL) != 0) {
        printf("FAIL: pthread_create: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }
    void *tret = NULL;
    if (pthread_join(tid, &tret) != 0) {
        printf("FAIL: pthread_join: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }
    if ((intptr_t)tret != 0) {
        printf("FAIL: CLONE_THREAD thread did not inherit {CPU 0}\n");
        printf("TEST FAILED\n");
        return 1;
    }
    printf("PASS: CLONE_THREAD thread inherited {CPU 0}\n");

    printf("TEST PASSED\n");
    return 0;
}
