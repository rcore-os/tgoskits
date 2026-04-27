/*
 * test_shm_deadlock.c - Stress test for SHM_MANAGER/shm_inner lock ordering.
 *
 * BUG-021: sys_shmget holds SHM_MANAGER then locks shm_inner.
 *          The buggy sys_shmat/sys_shmdt path held shm_inner first and then
 *          tried to lock SHM_MANAGER. Under SMP this forms an AB/BA deadlock.
 *
 * Strategy: start the shmat/shmdt worker first and make it attach a larger
 * segment on x86_64, so it keeps shm_inner long enough for the shmget worker
 * to grab SHM_MANAGER. The old lock order deadlocks reliably; the fixed lock
 * order serializes the two paths and the workers exit cleanly.
 *
 * This test uses clone() directly since musl pthreads may or may not work on
 * StarryOS. clone(CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND) creates
 * a thread sharing the address space.
 */
#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/ipc.h>
#include <sys/shm.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <sched.h>
#include <unistd.h>

/* Shared state between threads */
static volatile int g_running = 1;
static volatile int g_shmid = -1;
static volatile int g_deadlock_detected = 0;
static volatile int g_shmat_started = 0;

/* Thread stack size */
#define STACK_SIZE (64 * 1024)

#if defined(__x86_64__)
#define SHM_TEST_SIZE (32 * 1024 * 1024)
#define SHM_RACE_USEC 1500000
#define SHM_ALARM_SEC 10
#else
#define SHM_TEST_SIZE 4096
#define SHM_RACE_USEC 1000000
#define SHM_ALARM_SEC 3
#endif

/*
 * Thread 1: repeatedly call shmget with the same key.
 * This acquires SHM_MANAGER -> shm_inner (the "forward" order).
 */
static int shmget_thread(void *arg) {
    (void)arg;

    while (g_running && !g_shmat_started) {
        sched_yield();
    }

    while (g_running) {
        int id = shmget(42, SHM_TEST_SIZE, IPC_CREAT | 0666);
        if (id >= 0) {
            g_shmid = id;
        }
        sched_yield();
    }
    return 0;
}

/*
 * Thread 2: repeatedly call shmat/shmdt.
 * The buggy version acquired shm_inner before SHM_MANAGER. Starting this
 * worker first biases the race toward the old AB/BA lock order.
 */
static int shmat_thread(void *arg) {
    (void)arg;

    while (g_running) {
        int id = g_shmid;
        if (id >= 0) {
            g_shmat_started = 1;
            void *p = shmat(id, NULL, 0);
            if (p != (void *)-1) {
                shmdt(p);
            }
        }
        sched_yield();
    }
    return 0;
}

static int watchdog_thread(void *arg) {
    (void)arg;

    for (int i = 0; i < SHM_ALARM_SEC * 10; i++) {
        usleep(100000);
        if (!g_running) {
            return 0;
        }
    }

    g_deadlock_detected = 1;
    printf("  FAIL | test_shm_deadlock.c | concurrent_shmget_shmat"
           " (TIMEOUT after %ds - probable deadlock in SHM lock ordering)\n",
           SHM_ALARM_SEC);
    _exit(1);
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("shm_deadlock");

    /* --- concurrent_shmget_shmat --- */
    {
        /* Create initial segment so both threads have something to work with */
        g_shmid = shmget(42, SHM_TEST_SIZE, IPC_CREAT | 0666);
        CHECK(g_shmid >= 0, "initial shmget");

        if (g_shmid >= 0) {
            /* Allocate stacks for clone threads */
            void *stack1 = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            void *stack2 = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            void *stack3 = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);

            CHECK(stack1 != MAP_FAILED, "stack1 mmap");
            CHECK(stack2 != MAP_FAILED, "stack2 mmap");
            CHECK(stack3 != MAP_FAILED, "stack3 mmap");

            if (stack1 != MAP_FAILED && stack2 != MAP_FAILED &&
                stack3 != MAP_FAILED) {
                int flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND;

                /* clone() takes top of stack (stack grows down) */
                int tid3 = clone(watchdog_thread, (char *)stack3 + STACK_SIZE,
                                 flags, NULL);
                CHECK(tid3 >= 0, "clone watchdog_thread");

                int tid2 = clone(shmat_thread, (char *)stack2 + STACK_SIZE,
                                 flags, NULL);
                CHECK(tid2 >= 0, "clone shmat_thread");

                while (tid2 >= 0 && !g_shmat_started) {
                    sched_yield();
                }

                int tid1 = clone(shmget_thread, (char *)stack1 + STACK_SIZE,
                                 flags, NULL);
                CHECK(tid1 >= 0, "clone shmget_thread");

                if (tid1 >= 0 && tid2 >= 0 && tid3 >= 0) {
                    /*
                     * Let the threads race. If a deadlock occurs,
                     * the watchdog worker will print FAIL.
                     */
                    usleep(SHM_RACE_USEC);
                    g_running = 0;

                    /* Wait for threads to finish */
                    int status;
                    waitpid(tid1, &status, __WALL);
                    waitpid(tid2, &status, __WALL);
                    waitpid(tid3, &status, __WALL);

                    CHECK(!g_deadlock_detected, "no deadlock detected");
                } else {
                    g_running = 0;
                }

                /* Cleanup */
                if (stack1 != MAP_FAILED) munmap(stack1, STACK_SIZE);
                if (stack2 != MAP_FAILED) munmap(stack2, STACK_SIZE);
                if (stack3 != MAP_FAILED) munmap(stack3, STACK_SIZE);
            }

            /* Remove the shared memory segment */
            shmctl(g_shmid, IPC_RMID, NULL);
        }
    }

    TEST_DONE();
}
