#define _GNU_SOURCE

#include <errno.h>
#include <sched.h>
#include <stdint.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

enum {
    WORKER_COUNT = 8,
    SAMPLE_COUNT = 20000,
    TARGET_CPU_COUNT = 4,
};

struct placement_barrier {
    _Atomic int ready;
    _Atomic int start;
};

static void fail_errno(const char *message)
{
    printf("FAIL: %s: errno=%d (%s)\n", message, errno, strerror(errno));
    exit(1);
}

static void read_exact(int fd, void *buffer, size_t length)
{
    size_t offset = 0;
    while (offset < length) {
        ssize_t count = read(fd, (char *)buffer + offset, length - offset);
        if (count < 0 && errno == EINTR) {
            continue;
        }
        if (count <= 0) {
            fail_errno("read worker result");
        }
        offset += (size_t)count;
    }
}

static void write_exact(int fd, const void *buffer, size_t length)
{
    size_t offset = 0;
    while (offset < length) {
        ssize_t count = write(fd, (const char *)buffer + offset, length - offset);
        if (count < 0 && errno == EINTR) {
            continue;
        }
        if (count <= 0) {
            fail_errno("write worker result");
        }
        offset += (size_t)count;
    }
}

static cpu_set_t select_test_cpus(void)
{
    cpu_set_t allowed;
    if (sched_getaffinity(0, sizeof(allowed), &allowed) != 0) {
        fail_errno("sched_getaffinity");
    }

    cpu_set_t selected;
    CPU_ZERO(&selected);
    int selected_count = 0;
    for (int cpu = 0; cpu < CPU_SETSIZE && selected_count < TARGET_CPU_COUNT; cpu++) {
        if (CPU_ISSET(cpu, &allowed)) {
            CPU_SET(cpu, &selected);
            selected_count++;
        }
    }

    if (selected_count < 2) {
        printf("FAIL: SMP placement test requires at least two allowed CPUs\n");
        exit(1);
    }
    return selected;
}

static void run_worker(struct placement_barrier *barrier, int result_fd)
{
    /*
     * Keep every child runnable until all initial placements are observable.
     * A blocking pipe barrier would test wakeup placement instead.
     */
    atomic_fetch_add_explicit(&barrier->ready, 1, memory_order_release);
    while (!atomic_load_explicit(&barrier->start, memory_order_acquire)) {
        sched_yield();
    }

    cpu_set_t observed;
    CPU_ZERO(&observed);
    for (int sample = 0; sample < SAMPLE_COUNT; sample++) {
        unsigned int cpu = 0;
        if (syscall(SYS_getcpu, &cpu, NULL, NULL) != 0) {
            fail_errno("getcpu");
        }
        if (cpu >= CPU_SETSIZE) {
            printf("FAIL: getcpu returned out-of-range CPU %u\n", cpu);
            exit(1);
        }
        CPU_SET((int)cpu, &observed);
        sched_yield();
    }

    write_exact(result_fd, &observed, sizeof(observed));
    _exit(0);
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("SMP new-task placement regression\n");

    cpu_set_t selected = select_test_cpus();
    if (sched_setaffinity(0, sizeof(selected), &selected) != 0) {
        fail_errno("sched_setaffinity");
    }

    struct placement_barrier *barrier =
        mmap(NULL, sizeof(*barrier), PROT_READ | PROT_WRITE,
             MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (barrier == MAP_FAILED) {
        fail_errno("mmap placement barrier");
    }

    int result_pipe[2];
    if (pipe(result_pipe) != 0) {
        fail_errno("pipe");
    }

    pid_t workers[WORKER_COUNT];
    for (int worker = 0; worker < WORKER_COUNT; worker++) {
        pid_t pid = fork();
        if (pid < 0) {
            fail_errno("fork");
        }
        if (pid == 0) {
            close(result_pipe[0]);
            run_worker(barrier, result_pipe[1]);
        }
        workers[worker] = pid;
    }

    close(result_pipe[1]);

    while (atomic_load_explicit(&barrier->ready, memory_order_acquire) <
           WORKER_COUNT) {
        sched_yield();
    }
    atomic_store_explicit(&barrier->start, 1, memory_order_release);

    cpu_set_t observed;
    CPU_ZERO(&observed);
    for (int worker = 0; worker < WORKER_COUNT; worker++) {
        cpu_set_t worker_observed;
        read_exact(result_pipe[0], &worker_observed, sizeof(worker_observed));
        CPU_OR(&observed, &observed, &worker_observed);
    }
    close(result_pipe[0]);

    for (int worker = 0; worker < WORKER_COUNT; worker++) {
        int status = 0;
        if (waitpid(workers[worker], &status, 0) != workers[worker]) {
            fail_errno("waitpid");
        }
        if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            printf("FAIL: worker %d exited with status %#x\n", worker, status);
            return 1;
        }
    }

    if (munmap(barrier, sizeof(*barrier)) != 0) {
        fail_errno("munmap placement barrier");
    }

    int observed_count = CPU_COUNT(&observed);
    printf("Observed %d CPU(s) across %d concurrent workers\n",
           observed_count, WORKER_COUNT);
    if (observed_count < 2) {
        printf("FAIL: unpinned new tasks remained on one CPU\n");
        return 1;
    }

    printf("STARRY_SMP_NEW_TASK_PLACEMENT_PASSED\n");
    return 0;
}
