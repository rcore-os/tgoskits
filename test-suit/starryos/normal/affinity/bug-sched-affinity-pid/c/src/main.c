#define _GNU_SOURCE

#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static void fail(const char *msg)
{
    printf("TEST FAILED: %s: %s\n", msg, strerror(errno));
    fflush(stdout);
    abort();
}

static cpu_set_t single_cpu(int cpu)
{
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(cpu, &set);
    return set;
}

static void require_only_cpu(const cpu_set_t *set, int cpu, const char *msg)
{
    for (int i = 0; i < CPU_SETSIZE; i++) {
        int actual = CPU_ISSET(i, set) ? 1 : 0;
        int expected = i == cpu ? 1 : 0;
        if (actual != expected) {
            printf("TEST FAILED: %s\n", msg);
            abort();
        }
    }
}

int main(void)
{
    setbuf(stdout, NULL);
    printf("TEST START: sched affinity respects pid\n");

    cpu_set_t parent_cpu0 = single_cpu(0);
    if (sched_setaffinity(0, sizeof(parent_cpu0), &parent_cpu0) != 0) {
        fail("set parent affinity to CPU0");
    }

    pid_t child = fork();
    if (child < 0) {
        fail("fork");
    }
    if (child == 0) {
        for (;;) {
            sched_yield();
        }
        _exit(0);
    }

    cpu_set_t child_cpu1 = single_cpu(1);
    if (sched_setaffinity(child, sizeof(child_cpu1), &child_cpu1) != 0) {
        kill(child, SIGKILL);
        fail("set child affinity to CPU1");
    }

    cpu_set_t observed_child;
    CPU_ZERO(&observed_child);
    if (sched_getaffinity(child, sizeof(observed_child), &observed_child) != 0) {
        kill(child, SIGKILL);
        fail("get child affinity");
    }
    require_only_cpu(&observed_child, 1, "child affinity should be CPU1");

    cpu_set_t observed_parent;
    CPU_ZERO(&observed_parent);
    if (sched_getaffinity(0, sizeof(observed_parent), &observed_parent) != 0) {
        kill(child, SIGKILL);
        fail("get parent affinity");
    }
    require_only_cpu(&observed_parent, 0, "parent affinity should stay CPU0");

    kill(child, SIGKILL);
    waitpid(child, NULL, 0);

    printf("TEST PASSED\n");
    return 0;
}
