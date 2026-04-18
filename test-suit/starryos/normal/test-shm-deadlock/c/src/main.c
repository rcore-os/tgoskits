/*
 * test_shm_deadlock.c — Stress test for SHM_MANAGER/shm_inner lock ordering.
 *
 * BUG-021: sys_shmget holds SHM_MANAGER then locks shm_inner.
 *          sys_shmat/sys_shmdt hold shm_inner then lock SHM_MANAGER.
 *          Under concurrent execution, this is an AB/BA deadlock.
 *
 * Strategy: Spawn two threads doing concurrent shmget and shmat/shmdt
 * on the same key. If the deadlock triggers, the threads hang and
 * alarm() fires, reporting FAIL. If they complete, PASS (but the bug
 * may still exist — deadlocks are probabilistic).
 *
 * This test uses clone() directly since musl pthreads may or may not
 * work on StarryOS. clone(CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND)
 * creates a thread sharing the address space.
 */
#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/ipc.h>
#include <sys/shm.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <sched.h>
#include <unistd.h>
#include <signal.h>
#include <sys/syscall.h>

/* Shared state between threads */
static volatile int g_running = 1;
static volatile int g_shmid = -1;
static volatile int g_deadlock_detected = 0;

/* Thread stack size */
#define STACK_SIZE (64 * 1024)

static void alarm_handler(int sig) {
    (void)sig;
    g_deadlock_detected = 1;
    /* Force exit — if we got here, threads are deadlocked */
    printf("  FAIL | test_shm_deadlock.c | concurrent_shmget_shmat"
           " (TIMEOUT after 3s — probable deadlock in SHM lock ordering)\n");
    _exit(1);
}

/*
 * Thread 1: repeatedly call shmget with the same key.
 * This acquires SHM_MANAGER -> shm_inner (the "forward" order).
 */
static int shmget_thread(void *arg) {
    (void)arg;
    while (g_running) {
        int id = shmget(42, 4096, IPC_CREAT | 0666);
        if (id >= 0) {
            g_shmid = id;
        }
        /* Yield to give the other thread a chance */
        sched_yield();
    }
    return 0;
}

/*
 * Thread 2: repeatedly call shmat/shmdt.
 * This acquires shm_inner -> SHM_MANAGER in the buggy version (the "reverse"
 * order), which causes an AB/BA deadlock with sys_shmget.
 */
static int shmat_thread(void *arg) {
    (void)arg;
    while (g_running) {
        int id = g_shmid;
        if (id >= 0) {
            void *p = shmat(id, NULL, 0);
            if (p != (void *)-1) {
                shmdt(p);
            }
        }
        sched_yield();
    }
    return 0;
}

int main(void)
{
    TEST_START("shm_deadlock");

    /* --- concurrent_shmget_shmat --- */
    {
        /* Set alarm for deadlock detection */
        struct sigaction sa;
        sa.sa_handler = alarm_handler;
        sa.sa_flags = 0;
        sigemptyset(&sa.sa_mask);
        sigaction(SIGALRM, &sa, NULL);
        alarm(3);

        /* Create initial segment so both threads have something to work with */
        g_shmid = shmget(42, 4096, IPC_CREAT | 0666);
        CHECK(g_shmid >= 0, "initial shmget");

        if (g_shmid >= 0) {
            /* Allocate stacks for clone threads */
            void *stack1 = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            void *stack2 = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);

            CHECK(stack1 != MAP_FAILED, "stack1 mmap");
            CHECK(stack2 != MAP_FAILED, "stack2 mmap");

            if (stack1 != MAP_FAILED && stack2 != MAP_FAILED) {
                int flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND;

                /* clone() takes top of stack (stack grows down) */
                int tid1 = clone(shmget_thread, (char *)stack1 + STACK_SIZE,
                                 flags, NULL);
                int tid2 = clone(shmat_thread, (char *)stack2 + STACK_SIZE,
                                 flags, NULL);

                CHECK(tid1 >= 0, "clone shmget_thread");
                CHECK(tid2 >= 0, "clone shmat_thread");

                if (tid1 >= 0 && tid2 >= 0) {
                    /*
                     * Let the threads race for ~1 second.
                     * If a deadlock occurs, alarm(3) will fire.
                     */
                    usleep(1000000); /* 1 second */
                    g_running = 0;

                    /* Wait for threads to finish */
                    int status;
                    waitpid(tid1, &status, __WALL);
                    waitpid(tid2, &status, __WALL);

                    CHECK(!g_deadlock_detected, "no deadlock detected");
                } else {
                    g_running = 0;
                }

                /* Cancel alarm */
                alarm(0);

                /* Cleanup */
                if (stack1 != MAP_FAILED) munmap(stack1, STACK_SIZE);
                if (stack2 != MAP_FAILED) munmap(stack2, STACK_SIZE);
            }

            /* Remove the shared memory segment */
            shmctl(g_shmid, IPC_RMID, NULL);
        }
    }

    TEST_DONE();
}
