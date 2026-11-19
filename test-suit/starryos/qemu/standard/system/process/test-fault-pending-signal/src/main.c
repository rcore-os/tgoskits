/*
 * test-fault-pending-signal: a synchronous SIGSEGV must still terminate
 * the process when a lower-numbered signal is concurrently pending on
 * the same thread's *thread-level* pending queue.
 *
 * The kernel binds a fault-dump request to the faulting signo via
 * `fault_dump_signo` (an AtomicU8 set by `raise_signal_fatal`).
 * `check_signals` clears that slot only when the delivered signo
 * matches, using `compare_exchange(signo, 0)`. The previous
 * `AtomicBool` design risked having a lower-numbered queued signal
 * (e.g. SIGUSR1) consume the dump flag during its handler-only delivery
 * before the real fault signal got its turn.
 *
 * `check_signals_slow_with` drains the *thread-level* `self.pending`
 * queue before the process-level queue, so to truly exercise the
 * ordering we must place SIGUSR1 in the thread-level queue. The way
 * to do that from userspace is `tgkill(tgid, tid, SIGUSR1)`. Plain
 * `kill(pid, SIGUSR1)` routes through `send_signal_to_process()` and
 * lands in the process-level queue instead, which does not exercise
 * the race.
 *
 * The dump's printed register state is visible in the QEMU serial log
 * captured by CI but is not asserted here (no userspace facility to
 * read kernel logs).
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
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static atomic_int g_usr1_count;
static atomic_int g_stop_peer;
static pid_t g_main_pid;
static pid_t g_main_tid;

#define REQUIRED_USR1_DELIVERIES 8
#define POST_READY_SIGUSR1_BURST 128

static pid_t my_gettid(void)
{
    return (pid_t)syscall(SYS_gettid);
}

static int my_tgkill(pid_t tgid, pid_t tid, int sig)
{
    return (int)syscall(SYS_tgkill, tgid, tid, sig);
}

static void usr1_handler(int sig)
{
    (void)sig;
    atomic_fetch_add(&g_usr1_count, 1);
}

static void *peer_signaler(void *arg)
{
    (void)arg;
    /* Send SIGUSR1 at the main thread's *thread-level* pending queue
     * via tgkill, so it is dequeued ahead of (or interleaved with) the
     * synchronous SIGSEGV the main thread is about to raise. Keep the
     * post-ready stream bounded: a normal CI case must not rely on an
     * infinite lower-numbered signal flood, because single-vCPU QEMU can
     * otherwise keep SIGUSR1 ahead of SIGSEGV long enough to trip the
     * case timeout instead of proving the kernel invariant. */
    int post_ready = 0;
    while (!atomic_load(&g_stop_peer)) {
        my_tgkill(g_main_pid, g_main_tid, SIGUSR1);
        if (atomic_load(&g_usr1_count) >= REQUIRED_USR1_DELIVERIES
            && ++post_ready >= POST_READY_SIGUSR1_BURST) {
            break;
        }
        /* tiny pause to let the handler run and re-arm the pending
         * set, instead of hard-spinning the cpu */
        struct timespec ts = {0, 1000};
        nanosleep(&ts, NULL);
    }
    return NULL;
}

int main(void)
{
    TEST_START("fault pending signal order");

    pid_t pid = fork();
    CHECK(pid >= 0, "fork crash child");
    if (pid < 0) {
        TEST_DONE();
    }

    if (pid == 0) {
        /* Child: install a USER handler for SIGUSR1 so it goes through
         * the NoFurtherAction path in `check_signals` rather than
         * terminating us. SA_NODEFER + SA_RESTART keep the kernel from
         * masking SIGUSR1 during its own handler so the next
         * tgkill can land again. */
        struct sigaction sa = {0};
        sa.sa_handler = usr1_handler;
        sa.sa_flags   = SA_NODEFER | SA_RESTART;
        sigemptyset(&sa.sa_mask);
        if (sigaction(SIGUSR1, &sa, NULL) != 0) {
            _exit(99);
        }

        g_main_pid = getpid();
        g_main_tid = my_gettid();

        pthread_t peer;
        if (pthread_create(&peer, NULL, peer_signaler, NULL) != 0) {
            _exit(98);
        }

        /* Let the peer get a few SIGUSR1s in so at least one is
         * pending or actively delivering on this thread's
         * thread-level queue when we fault. */
        for (int i = 0; i < 200; ++i) {
            if (atomic_load(&g_usr1_count) >= REQUIRED_USR1_DELIVERIES) break;
            struct timespec ts = {0, 1000 * 1000};
            nanosleep(&ts, NULL);
        }

        /* Trigger the synchronous fault. The kernel must:
         *   1. raise_signal_fatal stores SIGSEGV in fault_dump_signo
         *   2. check_signals_slow_with drains thread-level pending
         *      (SIGUSR1 from tgkill) before process-level
         *   3. SIGUSR1 delivery (NoFurtherAction) leaves the slot alone
         *      because the compare_exchange compares against SIGUSR1,
         *      not SIGSEGV
         *   4. SIGSEGV delivery (Terminate/CoreDump) matches the slot,
         *      dumps register state, exits
         */
        volatile int *p = (volatile int *)0;
        *p = 42;

        /* Unreachable on a working kernel. Exit cleanly so the parent
         * can flag this as a bug. */
        atomic_store(&g_stop_peer, 1);
        _exit(0);
    }

    int status = 0;
    pid_t got = waitpid(pid, &status, 0);
    CHECK(got == pid, "waitpid returned crash child");

    int by_signal = WIFSIGNALED(status) && WTERMSIG(status) == SIGSEGV;
    int by_exit_segv = WIFEXITED(status)
                       && (WEXITSTATUS(status) == SIGSEGV
                           || WEXITSTATUS(status) == 128 + SIGSEGV);
    CHECK(by_signal || by_exit_segv,
          "child terminated by SIGSEGV despite concurrent SIGUSR1 pending");
    CHECK(!(WIFEXITED(status) && WEXITSTATUS(status) == 0),
          "child did not exit cleanly past the fault");

    TEST_DONE();
}
