/*
 * test_shm_deadlock.c - Stress the SHM_MANAGER -> shm_inner lock order.
 *
 * BUG-021: sys_shmget and the shmat/shmdt bookkeeping path must acquire the
 * shared-memory locks in one order.  The old AB/BA implementation could
 * deadlock when these operations ran concurrently.
 *
 * This test deliberately uses finite work and bounded joins.  A wall-clock
 * alarm is not a sound liveness oracle on a preemptive guest: a long page
 * table transaction can delay timer delivery even though both workers make
 * progress.  The operation counters and bounded wait still turn a real
 * deadlock into a deterministic failure, without introducing a second timer
 * or signal lifecycle into the test.
 */
#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/ipc.h>
#include <sys/shm.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <signal.h>
#include <sched.h>
#include <unistd.h>

#define STACK_SIZE (64 * 1024)
#define WORKER_OPS 4
#define START_SPINS 500000u
#define JOIN_SPINS 500000u

#if defined(__x86_64__)
#define SHM_TEST_SIZE (32 * 1024 * 1024)
#else
#define SHM_TEST_SIZE (256 * 1024)
#endif

/* Shared state is accessed with the compiler's acquire/release atomics. */
static volatile int g_running;
static volatile int g_shmid;
static volatile int g_shmat_started;
static volatile int g_shmget_started;
static volatile int g_shmat_ops;
static volatile int g_shmget_ops;
static volatile int g_worker_error;

static int atomic_load_int(const volatile int *value)
{
    return __atomic_load_n(value, __ATOMIC_ACQUIRE);
}

static void atomic_store_int(volatile int *value, int new_value)
{
    __atomic_store_n(value, new_value, __ATOMIC_RELEASE);
}

static void mark_worker_error(void)
{
    atomic_store_int(&g_worker_error, 1);
    atomic_store_int(&g_running, 0);
}

/* Worker 1 follows SHM_MANAGER -> shm_inner through shmget(). */
static int shmget_thread(void *arg)
{
    (void)arg;
    atomic_store_int(&g_shmget_started, 1);

    for (int i = 0; i < WORKER_OPS && atomic_load_int(&g_running); i++) {
        int id = shmget(42, SHM_TEST_SIZE, IPC_CREAT | 0666);
        if (id < 0) {
            mark_worker_error();
            break;
        }
        atomic_store_int(&g_shmid, id);
        __atomic_fetch_add(&g_shmget_ops, 1, __ATOMIC_RELAXED);
        sched_yield();
    }
    return atomic_load_int(&g_worker_error) ? 1 : 0;
}

/* Worker 2 exercises attach/detach and its manager bookkeeping. */
static int shmat_thread(void *arg)
{
    (void)arg;
    atomic_store_int(&g_shmat_started, 1);

    /* Make the two lock paths overlap instead of relying on scheduling luck. */
    for (unsigned i = 0;
         i < START_SPINS && !atomic_load_int(&g_shmget_started) &&
         atomic_load_int(&g_running);
         i++) {
        sched_yield();
    }

    for (int i = 0; i < WORKER_OPS && atomic_load_int(&g_running); i++) {
        int id = atomic_load_int(&g_shmid);
        if (id < 0) {
            mark_worker_error();
            break;
        }

        void *address = shmat(id, NULL, 0);
        if (address == (void *)-1) {
            mark_worker_error();
            break;
        }
        if (shmdt(address) < 0) {
            mark_worker_error();
            break;
        }
        __atomic_fetch_add(&g_shmat_ops, 1, __ATOMIC_RELAXED);
        sched_yield();
    }
    return atomic_load_int(&g_worker_error) ? 1 : 0;
}

/* Wait without sleeping forever if a buggy kernel really deadlocks. */
static int wait_child_bounded(pid_t child, int *status)
{
    for (unsigned i = 0; i < JOIN_SPINS; i++) {
        pid_t result = waitpid(child, status, WNOHANG | __WALL);
        if (result == child) {
            return 0;
        }
        if (result < 0) {
            if (errno == EINTR) {
                continue;
            }
            return errno;
        }
        sched_yield();
    }
    return ETIMEDOUT;
}

static void stop_child(pid_t child)
{
    if (child <= 0) {
        return;
    }
    (void)kill(child, SIGKILL);
    int status = 0;
    (void)wait_child_bounded(child, &status);
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("shm_deadlock");

    pid_t shmat_child = -1;
    pid_t shmget_child = -1;
    void *shmat_stack = MAP_FAILED;
    void *shmget_stack = MAP_FAILED;
    int status_shmat = 0;
    int status_shmget = 0;

    atomic_store_int(&g_running, 1);
    atomic_store_int(&g_shmid, -1);
    atomic_store_int(&g_shmat_started, 0);
    atomic_store_int(&g_shmget_started, 0);
    atomic_store_int(&g_shmat_ops, 0);
    atomic_store_int(&g_shmget_ops, 0);
    atomic_store_int(&g_worker_error, 0);

    int shmid = shmget(42, SHM_TEST_SIZE, IPC_CREAT | 0666);
    CHECK(shmid >= 0, "initial shmget");
    if (shmid < 0) {
        TEST_DONE();
    }
    atomic_store_int(&g_shmid, shmid);

    shmat_stack = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    shmget_stack = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(shmat_stack != MAP_FAILED, "shmat worker stack");
    CHECK(shmget_stack != MAP_FAILED, "shmget worker stack");
    if (shmat_stack == MAP_FAILED || shmget_stack == MAP_FAILED) {
        goto cleanup;
    }

    const int flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | SIGCHLD;
    shmat_child = clone(shmat_thread, (char *)shmat_stack + STACK_SIZE,
                        flags, NULL);
    CHECK(shmat_child > 0, "clone shmat worker");
    if (shmat_child <= 0) {
        goto cleanup;
    }

    int started = 0;
    for (unsigned i = 0; i < START_SPINS; i++) {
        if (atomic_load_int(&g_shmat_started)) {
            started = 1;
            break;
        }
        sched_yield();
    }
    CHECK(started, "shmat worker starts before shmget worker");
    if (!started) {
        goto cleanup;
    }

    shmget_child = clone(shmget_thread, (char *)shmget_stack + STACK_SIZE,
                         flags, NULL);
    CHECK(shmget_child > 0, "clone shmget worker");
    if (shmget_child <= 0) {
        goto cleanup;
    }

    int wait_result_shmat = wait_child_bounded(shmat_child, &status_shmat);
    int wait_result_shmget = wait_child_bounded(shmget_child, &status_shmget);
    atomic_store_int(&g_running, 0);
    CHECK(wait_result_shmat == 0, "shmat worker completes without deadlock");
    CHECK(wait_result_shmget == 0, "shmget worker completes without deadlock");
    CHECK(wait_result_shmat == 0 && WIFEXITED(status_shmat) &&
          WEXITSTATUS(status_shmat) == 0, "shmat worker exits successfully");
    CHECK(wait_result_shmget == 0 && WIFEXITED(status_shmget) &&
          WEXITSTATUS(status_shmget) == 0, "shmget worker exits successfully");
    CHECK(!atomic_load_int(&g_worker_error), "workers report no SHM errors");
    CHECK(atomic_load_int(&g_shmat_ops) > 0, "shmat/shmdt operations completed");
    CHECK(atomic_load_int(&g_shmget_ops) > 0, "shmget operations completed");

cleanup:
    atomic_store_int(&g_running, 0);
    stop_child(shmat_child);
    stop_child(shmget_child);
    if (shmat_stack != MAP_FAILED) {
        munmap(shmat_stack, STACK_SIZE);
    }
    if (shmget_stack != MAP_FAILED) {
        munmap(shmget_stack, STACK_SIZE);
    }
    (void)shmctl(shmid, IPC_RMID, NULL);

    TEST_DONE();
}
