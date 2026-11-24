#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

struct shared_state {
    _Atomic int victim_ready;
    _Atomic int setter_ready;
    _Atomic int migrate;
    _Atomic int completed;
    _Atomic int entered;
    pid_t victim;
    int result;
    int error;
    unsigned int cpu;
};

static int pin(pid_t pid, int cpu)
{
    cpu_set_t mask;
    CPU_ZERO(&mask);
    CPU_SET(cpu, &mask);
    return (int)syscall(SYS_sched_setaffinity, pid, sizeof(mask), &mask);
}

static int wait_flag(_Atomic int *flag)
{
    struct timespec start, now;
    if (clock_gettime(CLOCK_MONOTONIC, &start) != 0)
        return -1;
    while (!atomic_load_explicit(flag, memory_order_acquire)) {
        if (clock_gettime(CLOCK_MONOTONIC, &now) != 0)
            return -1;
        if (now.tv_sec - start.tv_sec >= 5) {
            errno = ETIMEDOUT;
            return -1;
        }
        syscall(SYS_sched_yield);
    }
    return 0;
}

static int reap(pid_t pid)
{
    int status;
    return waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

int main(void)
{
    cpu_set_t original;
    struct sched_param saved, fifo = {.sched_priority = 90};
    int cpus[2], count = 0, gate[2] = {-1, -1}, result = 1;
    int policy = (int)syscall(SYS_sched_getscheduler, 0);
    pid_t victim = -1, setter = -1;
    const char *failure = "initialize queued affinity fixture";
    CPU_ZERO(&original);
    if (policy < 0 || syscall(SYS_sched_getparam, 0, &saved) != 0 ||
        syscall(SYS_sched_getaffinity, 0, sizeof(original), &original) < 0) {
        perror(failure);
        return 1;
    }
    for (int cpu = 0; cpu < CPU_SETSIZE && count < 2; ++cpu) {
        if (CPU_ISSET(cpu, &original))
            cpus[count++] = cpu;
    }
    if (count != 2) {
        fprintf(stderr, "queued affinity regression requires two allowed CPUs\n");
        return 1;
    }
    struct shared_state *shared = mmap(NULL, sizeof(*shared), PROT_READ | PROT_WRITE,
                                      MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (shared == MAP_FAILED)
        return 1;
    memset(shared, 0, sizeof(*shared));
    atomic_init(&shared->victim_ready, 0);
    atomic_init(&shared->setter_ready, 0);
    atomic_init(&shared->migrate, 0);
    atomic_init(&shared->completed, 0);
    atomic_init(&shared->entered, 0);
    if (pipe(gate) != 0)
        goto cleanup;
    victim = fork();
    if (victim < 0)
        goto cleanup;
    if (victim == 0) {
        char byte;
        if (pin(0, cpus[0]) != 0)
            _exit(11);
        atomic_store_explicit(&shared->victim_ready, 1, memory_order_release);
        if (read(gate[0], &byte, 1) != 1)
            _exit(12);
        if (syscall(SYS_getcpu, &shared->cpu, NULL, NULL) != 0)
            _exit(13);
        atomic_store_explicit(&shared->entered, 1, memory_order_release);
        _exit(0);
    }
    shared->victim = victim;
    setter = fork();
    if (setter < 0)
        goto cleanup;
    if (setter == 0) {
        if (pin(0, cpus[1]) != 0)
            _exit(21);
        atomic_store_explicit(&shared->setter_ready, 1, memory_order_release);
        if (wait_flag(&shared->migrate) != 0)
            _exit(22);
        shared->result = pin(shared->victim, cpus[1]);
        shared->error = errno;
        atomic_store_explicit(&shared->completed, 1, memory_order_release);
        _exit(0);
    }
    if (wait_flag(&shared->victim_ready) != 0 || wait_flag(&shared->setter_ready) != 0 ||
        pin(0, cpus[0]) != 0)
        goto cleanup;
    failure = "establish FIFO owner of the source CPU";
    if (syscall(SYS_sched_setscheduler, 0, SCHED_FIFO, &fifo) != 0)
        goto cleanup;
    /* Wake the Fair victim while the FIFO parent keeps CPU0. The target is
     * runnable but cannot execute or consume the pipe byte on the source CPU.
     * CPU1 must therefore update a queued, non-current task's affinity. */
    failure = "publish runnable victim";
    if (write(gate[1], "x", 1) != 1 ||
        atomic_load_explicit(&shared->entered, memory_order_acquire))
        goto cleanup;
    atomic_store_explicit(&shared->migrate, 1, memory_order_release);
    failure = "remote sched_setaffinity must complete for a queued target";
    if (wait_flag(&shared->completed) != 0)
        goto cleanup;
    if (shared->result != 0) {
        errno = shared->error;
        goto cleanup;
    }
    failure = "restore source CPU scheduling policy";
    if (syscall(SYS_sched_setscheduler, 0, policy, &saved) != 0)
        goto cleanup;
    failure = "victim must execute on the requested destination CPU";
    if (!reap(victim))
        goto cleanup;
    victim = -1;
    if (!atomic_load_explicit(&shared->entered, memory_order_acquire) ||
        shared->cpu != (unsigned int)cpus[1])
        goto cleanup;
    if (!reap(setter))
        goto cleanup;
    setter = -1;
    result = 0;
cleanup:
    if (result != 0)
        fprintf(stderr, "queued affinity FAIL: %s: errno=%d\n", failure, errno);
    if (syscall(SYS_sched_setscheduler, 0, policy, &saved) != 0 ||
        syscall(SYS_sched_setaffinity, 0, sizeof(original), &original) != 0)
        result = 1;
    if (victim > 0) {
        kill(victim, SIGKILL);
        waitpid(victim, NULL, 0);
    }
    if (setter > 0) {
        kill(setter, SIGKILL);
        waitpid(setter, NULL, 0);
    }
    if (gate[0] >= 0)
        close(gate[0]);
    if (gate[1] >= 0)
        close(gate[1]);
    munmap(shared, sizeof(*shared));
    if (result == 0)
        puts("QUEUED_AFFINITY_PASSED");
    return result;
}
