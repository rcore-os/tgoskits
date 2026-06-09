/*
 * test-fault-thread-routing: verify that a synchronous user-mode fault
 * (SIGSEGV from a null-pointer deref) is delivered to the *faulting*
 * thread, not to some other unmasked thread of the process. Mirrors
 * Linux's force_sig() contract.
 *
 * Approach:
 *   - fork() a child so the parent can wait() on the crash and report.
 *   - In the child, mmap a MAP_SHARED scratch region used as an out-of-band
 *     channel for the signal handler to record which thread actually ran
 *     the handler.
 *   - Spawn N pthread workers. Each worker records its own gettid()
 *     in `worker_tids[i]` and then either spins (passive) or, for one
 *     designated worker, dereferences NULL to take SIGSEGV.
 *   - Install a SIGSEGV handler with SA_NODEFER that writes
 *     gettid() into `observed_tid` and then re-raises by clearing the
 *     handler and returning, causing the default-fatal SIGSEGV to
 *     terminate the process. The parent waitpid()s the child, then
 *     reads the recorded tids from the shared region.
 *   - The fix is correct iff `observed_tid == worker_tids[FAULTING_IDX]`.
 *     Before the fix, the kernel routed the fault to the process and the
 *     signal manager picked an arbitrary thread (often main / a peer), so
 *     `observed_tid` could be the wrong worker or even the main thread.
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define NUM_WORKERS 4
#define FAULTING_IDX 2

struct shared {
    pid_t worker_tids[NUM_WORKERS];
    atomic_int ready_count;
    atomic_int observed_tid;
};

static struct shared *g_sh;

static pid_t my_tid(void)
{
    return (pid_t)syscall(SYS_gettid);
}

static void segv_handler(int sig)
{
    (void)sig;
    /* Record which thread actually ran the handler — must equal the
     * worker that triggered the fault. */
    int expected = 0;
    atomic_compare_exchange_strong(&g_sh->observed_tid, &expected, my_tid());
    /* Exit the whole process cleanly here. Returning from the handler
     * would re-run the faulting instruction and rely on a second
     * SIGSEGV with SIG_DFL to tear the process down; that path varies
     * across kernels and can hang if the second delivery races other
     * threads. _exit with a distinguishable status proves the handler
     * ran, and the recorded tid proves it ran on the faulting thread. */
    _exit(77);
}

static void *worker(void *arg)
{
    int idx = (int)(intptr_t)arg;
    g_sh->worker_tids[idx] = my_tid();
    atomic_fetch_add(&g_sh->ready_count, 1);

    if (idx == FAULTING_IDX) {
        /* Wait until every other worker is sitting in pause() — they're
         * unmasked and reachable, so the old bug had the maximum chance
         * to misroute. */
        while (atomic_load(&g_sh->ready_count) < NUM_WORKERS) {
            sched_yield();
        }
        /* Deliberate null deref. */
        volatile int *p = (volatile int *)0;
        *p = 42;
    } else {
        /* Idle by blocking in pause(). Cooperative sched_yield on
         * SMP=1 TCG can starve the faulting worker when N-1 peers are
         * all runnable; blocking workers keeps exactly one thread on
         * the runqueue (the faulter) until the fault fires. */
        pause();
    }
    return NULL;
}

int main(void)
{
    TEST_START("fault thread routing");

    g_sh = mmap(NULL, sizeof(*g_sh), PROT_READ | PROT_WRITE,
                MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    CHECK(g_sh != MAP_FAILED, "mmap shared scratch");
    if (g_sh == MAP_FAILED) {
        TEST_DONE();
    }
    memset(g_sh, 0, sizeof(*g_sh));

    pid_t pid = fork();
    CHECK(pid >= 0, "fork crash child");
    if (pid < 0) {
        TEST_DONE();
    }

    if (pid == 0) {
        /* Child: install handler, spawn workers, let FAULTING_IDX die. */
        struct sigaction sa = {0};
        sa.sa_handler = segv_handler;
        sa.sa_flags = SA_NODEFER;
        sigemptyset(&sa.sa_mask);
        if (sigaction(SIGSEGV, &sa, NULL) != 0) {
            _exit(99);
        }

        pthread_t tids[NUM_WORKERS];
        for (int i = 0; i < NUM_WORKERS; i++) {
            if (pthread_create(&tids[i], NULL, worker, (void *)(intptr_t)i) != 0) {
                _exit(98);
            }
        }
        /* Don't join — one of these threads is going to take a fatal
         * SIGSEGV and the kernel will tear the whole process down. */
        for (;;) {
            pause();
        }
    }

    /* Parent: wait for the child to exit via the handler's _exit(77). */
    int status = 0;
    pid_t got = waitpid(pid, &status, 0);
    CHECK(got == pid, "waitpid returned crash child");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 77,
          "child exited via SIGSEGV handler (status 77)");

    /* Confirm the handler observed the *faulting* thread. */
    int observed = atomic_load(&g_sh->observed_tid);
    pid_t expected = g_sh->worker_tids[FAULTING_IDX];
    CHECK(observed != 0, "SIGSEGV handler ran at least once");
    CHECK(observed == expected,
          "SIGSEGV delivered to the faulting thread (force_sig routing)");

    munmap(g_sh, sizeof(*g_sh));
    TEST_DONE();
}
