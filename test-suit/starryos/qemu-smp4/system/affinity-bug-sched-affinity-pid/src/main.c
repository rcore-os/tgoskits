#define _GNU_SOURCE

#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

#define ITERATIONS 20
#define WAIT_RETRIES 10000

static void fail_at(const char *msg, int iter)
{
    printf("TEST FAILED: iter=%d: %s: %s\n", iter, msg, strerror(errno));
    fflush(stdout);
    abort();
}

static void fail_msg_at(const char *msg, int iter)
{
    printf("TEST FAILED: iter=%d: %s\n", iter, msg);
    fflush(stdout);
    abort();
}

static void phase_at(int iter, const char *phase)
{
    printf("ITER %d %s\n", iter, phase);
    fflush(stdout);
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

static void reap_killed_child(pid_t child, int iter)
{
    int status;

    if (kill(child, SIGKILL) != 0) {
        fail_at("kill child", iter);
    }

    for (int i = 0; i < WAIT_RETRIES; i++) {
        pid_t waited = waitpid(child, &status, WNOHANG);
        if (waited == child) {
            return;
        }
        if (waited < 0) {
            fail_at("waitpid child", iter);
        }
        sched_yield();
    }

    fail_msg_at("child did not exit after SIGKILL", iter);
}

static void run_iteration(int iter)
{
    int parent_cpu = iter & 1;
    int child_cpu = parent_cpu ^ 1;

    cpu_set_t parent_set = single_cpu(parent_cpu);
    phase_at(iter, "parent-setaffinity begin");
    if (sched_setaffinity(0, sizeof(parent_set), &parent_set) != 0) {
        fail_at("set parent affinity", iter);
    }
    phase_at(iter, "parent-setaffinity done");

    pid_t child = fork();
    if (child < 0) {
        fail_at("fork", iter);
    }
    if (child == 0) {
        for (;;) {
            sched_yield();
        }
        _exit(0);
    }

    cpu_set_t child_set = single_cpu(child_cpu);
    phase_at(iter, "child-setaffinity begin");
    if (sched_setaffinity(child, sizeof(child_set), &child_set) != 0) {
        kill(child, SIGKILL);
        fail_at("set child affinity", iter);
    }
    phase_at(iter, "child-setaffinity done");

    cpu_set_t observed_child;
    CPU_ZERO(&observed_child);
    if (sched_getaffinity(child, sizeof(observed_child), &observed_child) != 0) {
        kill(child, SIGKILL);
        fail_at("get child affinity", iter);
    }
    require_only_cpu(&observed_child, child_cpu, "child affinity should match requested CPU");

    cpu_set_t observed_parent;
    CPU_ZERO(&observed_parent);
    if (sched_getaffinity(0, sizeof(observed_parent), &observed_parent) != 0) {
        kill(child, SIGKILL);
        fail_at("get parent affinity", iter);
    }
    require_only_cpu(&observed_parent, parent_cpu, "parent affinity should match requested CPU");

    phase_at(iter, "wait begin");
    reap_killed_child(child, iter);
    phase_at(iter, "wait done");
}

int main(void)
{
    setbuf(stdout, NULL);
    printf("TEST START: sched affinity respects pid\n");

    for (int i = 0; i < ITERATIONS; i++) {
        run_iteration(i);
    }

    printf("TEST PASSED\n");
    return 0;
}
