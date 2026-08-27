// Deterministic SMP coverage for shared-mm permission transitions.
//
// The worker first caches a writable translation on another CPU, then blocks
// in read(). The controller removes write permission and wakes that exact
// syscall, so the kernel-to-user copy must observe the new permission. After
// write permission is restored, the worker yields and writes through the same
// VA. A separate fork phase verifies that kernel user-copy resolves COW without
// modifying the parent's frame on both CPUs.
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

enum { PAGE_SIZE = 4096 };

struct shared_mm_probe {
    pthread_barrier_t phase;
    char *page;
    int wake_pipe[2];
    int worker_cpu;
    _Atomic pid_t worker_tid;
    _Atomic int worker_error;
    _Atomic int abort_write_probe;
    ssize_t protected_read_result;
    int protected_read_errno;
};

static int pin_to_cpu(int cpu)
{
    cpu_set_t affinity;
    CPU_ZERO(&affinity);
    CPU_SET(cpu, &affinity);
    if (sched_setaffinity(0, sizeof(affinity), &affinity) != 0)
        return -1;
    return sched_getcpu() == cpu ? 0 : -1;
}

static int cross_barrier(pthread_barrier_t *barrier)
{
    int result = pthread_barrier_wait(barrier);
    return result == 0 || result == PTHREAD_BARRIER_SERIAL_THREAD ? 0 : -1;
}

static int task_is_sleeping(pid_t tid)
{
    char path[64];
    int length = snprintf(path, sizeof(path), "/proc/self/task/%ld/status",
                          (long)tid);
    if (length < 0 || (size_t)length >= sizeof(path)) {
        errno = ENAMETOOLONG;
        return -1;
    }

    FILE *status = fopen(path, "r");
    if (status == NULL)
        return -1;

    char line[128];
    int sleeping = 0;
    while (fgets(line, sizeof(line), status) != NULL) {
        if (strncmp(line, "State:\tS", 8) == 0) {
            sleeping = 1;
            break;
        }
    }
    fclose(status);
    return sleeping;
}

static int wait_until_task_sleeps(pid_t tid)
{
    for (int attempt = 0; attempt < 500; ++attempt) {
        int sleeping = task_is_sleeping(tid);
        if (sleeping != 0)
            return sleeping;
        usleep(10000);
    }
    errno = ETIMEDOUT;
    return 0;
}

static void *shared_mm_worker(void *opaque)
{
    struct shared_mm_probe *probe = opaque;
    atomic_store_explicit(&probe->worker_tid, (pid_t)syscall(SYS_gettid),
                          memory_order_release);
    if (pin_to_cpu(probe->worker_cpu) != 0)
        atomic_store_explicit(&probe->worker_error, 1,
                              memory_order_release);

    // Publish a writable translation in the remote CPU's TLB.
    probe->page[0] = 'W';
    sched_yield();
    if (cross_barrier(&probe->phase) != 0) {
        atomic_store_explicit(&probe->worker_error, 2,
                              memory_order_release);
        return NULL;
    }

    // The controller observes this thread sleeping in /proc before changing
    // the PTE, then supplies one byte. copy_to_user must fault against
    // PROT_READ when this blocked syscall resumes.
    errno = 0;
    probe->protected_read_result = read(probe->wake_pipe[0], probe->page, 1);
    probe->protected_read_errno = errno;
    if (cross_barrier(&probe->phase) != 0) {
        atomic_store_explicit(&probe->worker_error, 3,
                              memory_order_release);
        return NULL;
    }

    // Wait until the controller has restored write permission.
    if (cross_barrier(&probe->phase) != 0) {
        atomic_store_explicit(&probe->worker_error, 4,
                              memory_order_release);
        return NULL;
    }
    if (!atomic_load_explicit(&probe->abort_write_probe,
                              memory_order_acquire)) {
        sched_yield();
        probe->page[0] = 'R';
    }
    if (cross_barrier(&probe->phase) != 0)
        atomic_store_explicit(&probe->worker_error, 5,
                              memory_order_release);
    return NULL;
}

static int shared_mm_permission_transition(int controller_cpu, int worker_cpu)
{
    struct shared_mm_probe probe = {
        .page = MAP_FAILED,
        .wake_pipe = {-1, -1},
        .worker_cpu = worker_cpu,
        .protected_read_result = -2,
    };
    int pipe_ready = 0;
    int barrier_ready = 0;
    int worker_ready = 0;
    int failed = 0;
    int completed = 0;
    int page_is_protected = 0;
    pthread_t worker;

    probe.page = mmap(NULL, PAGE_SIZE, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (probe.page == MAP_FAILED || pipe(probe.wake_pipe) != 0) {
        perror("shared-mm setup");
        goto cleanup;
    }
    pipe_ready = 1;
    if (pthread_barrier_init(&probe.phase, NULL, 2) != 0) {
        perror("shared-mm barrier setup");
        goto cleanup;
    }
    barrier_ready = 1;
    probe.page[0] = 'I';

    if (pthread_create(&worker, NULL, shared_mm_worker, &probe) != 0) {
        fprintf(stderr, "shared-mm worker setup failed\n");
        goto cleanup;
    }
    worker_ready = 1;
    if (pin_to_cpu(controller_cpu) != 0)
        failed = 1;
    if (cross_barrier(&probe.phase) != 0)
        failed = 1;

    pid_t worker_tid =
        atomic_load_explicit(&probe.worker_tid, memory_order_acquire);
    if (probe.page[0] != 'W' || worker_tid <= 0 ||
        atomic_load_explicit(&probe.worker_error, memory_order_acquire) != 0 ||
        wait_until_task_sleeps(worker_tid) != 1) {
        perror("shared-mm blocked-state verification");
        failed = 1;
    }
    if (mprotect(probe.page, PAGE_SIZE, PROT_READ) != 0) {
        perror("shared-mm protect phase");
        failed = 1;
    } else {
        page_is_protected = 1;
    }
    if (write(probe.wake_pipe[1], "X", 1) != 1) {
        // Closing the writer still releases a blocked read, so the thread can
        // finish the protocol and be joined on every failure path.
        close(probe.wake_pipe[1]);
        probe.wake_pipe[1] = -1;
        failed = 1;
    }
    if (cross_barrier(&probe.phase) != 0) {
        perror("shared-mm protect/wake phase");
        failed = 1;
    }
    if (probe.protected_read_result != -1 ||
        probe.protected_read_errno != EFAULT || probe.page[0] != 'W') {
        fprintf(stderr,
                "stale writable translation: read=%zd errno=%d value=%d\n",
                probe.protected_read_result, probe.protected_read_errno,
                probe.page[0]);
        failed = 1;
    }

    if (page_is_protected &&
        mprotect(probe.page, PAGE_SIZE, PROT_READ | PROT_WRITE) != 0) {
        perror("shared-mm restore phase");
        atomic_store_explicit(&probe.abort_write_probe, 1,
                              memory_order_release);
        failed = 1;
    }
    if (cross_barrier(&probe.phase) != 0 || cross_barrier(&probe.phase) != 0)
        failed = 1;

    if (pthread_join(worker, NULL) != 0)
        failed = 1;
    worker_ready = 0;
    if (atomic_load_explicit(&probe.worker_error, memory_order_acquire) != 0 ||
        (!atomic_load_explicit(&probe.abort_write_probe,
                               memory_order_acquire) &&
         probe.page[0] != 'R'))
        failed = 1;
    completed = 1;

cleanup:
    if (worker_ready) {
        if (probe.wake_pipe[1] >= 0)
            close(probe.wake_pipe[1]);
        pthread_join(worker, NULL);
    }
    if (barrier_ready)
        pthread_barrier_destroy(&probe.phase);
    if (pipe_ready) {
        close(probe.wake_pipe[0]);
        if (probe.wake_pipe[1] >= 0)
            close(probe.wake_pipe[1]);
    }
    if (probe.page != MAP_FAILED)
        munmap(probe.page, PAGE_SIZE);
    return failed || !completed ? -1 : 0;
}

static int cow_kernel_copy_on_cpu(int cpu)
{
    static const char payload[] = "MM-COW-OK";
    int data[2];
    if (pipe(data) != 0)
        return -1;

    char *page = mmap(NULL, PAGE_SIZE, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED)
        return -1;
    page[0] = 'P';

    pid_t child = fork();
    if (child == 0) {
        close(data[1]);
        if (pin_to_cpu(cpu) != 0)
            _exit(10);
        size_t received = 0;
        while (received < sizeof(payload)) {
            ssize_t count = read(data[0], page + received,
                                 sizeof(payload) - received);
            if (count <= 0)
                _exit(11);
            received += (size_t)count;
        }
        _exit(memcmp(page, payload, sizeof(payload)) == 0 ? 0 : 12);
    }
    if (child < 0)
        return -1;

    close(data[0]);
    int write_ok = write(data[1], payload, sizeof(payload)) ==
                   (ssize_t)sizeof(payload);
    close(data[1]);
    int status = 0;
    int wait_ok = waitpid(child, &status, 0) == child;
    int ok = write_ok && wait_ok && WIFEXITED(status) &&
             WEXITSTATUS(status) == 0 && page[0] == 'P';
    munmap(page, PAGE_SIZE);
    return ok ? 0 : -1;
}

int main(void)
{
    cpu_set_t allowed;
    if (sched_getaffinity(0, sizeof(allowed), &allowed) != 0) {
        perror("sched_getaffinity");
        return 1;
    }

    int cpus[2] = {-1, -1};
    for (int cpu = 0; cpu < CPU_SETSIZE && cpus[1] < 0; ++cpu) {
        if (!CPU_ISSET(cpu, &allowed))
            continue;
        if (cpus[0] < 0)
            cpus[0] = cpu;
        else
            cpus[1] = cpu;
    }
    if (cpus[1] < 0) {
        fprintf(stderr, "mm-transition-safety requires at least two CPUs\n");
        return 1;
    }

    printf("=== mm-transition-safety ===\n");
    if (shared_mm_permission_transition(cpus[0], cpus[1]) != 0) {
        fprintf(stderr, "FAIL: shared-mm permission transition\n");
        return 1;
    }
    printf("PASS: shared mm protect/block/wake/yield transition\n");

    for (size_t index = 0; index < 2; ++index) {
        if (cow_kernel_copy_on_cpu(cpus[index]) != 0) {
            fprintf(stderr, "FAIL: COW kernel-copy on CPU %d\n", cpus[index]);
            return 1;
        }
        printf("PASS: COW kernel-copy on CPU %d\n", cpus[index]);
    }

    printf("ALL TESTS PASSED\n");
    return 0;
}
