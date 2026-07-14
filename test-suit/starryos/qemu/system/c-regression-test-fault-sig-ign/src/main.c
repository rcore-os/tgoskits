/*
 * test-fault-sig-ign: synchronous-fault SIG_IGN must be ignored.
 *
 * Linux/POSIX: a SIGSEGV from a null-pointer dereference cannot be
 * silently swallowed by `signal(SIGSEGV, SIG_IGN)`. The kernel resets
 * the disposition to SIG_DFL before delivering the synchronous signal
 * (see Linux's force_sig_info()) so the process terminates anyway,
 * preventing an endless fault loop.
 *
 * Approach: fork a child, install SIG_IGN for SIGSEGV in the child,
 * deref NULL, and observe in the parent that the child died via the
 * signal rather than e.g. returning from the faulting instruction or
 * looping. Acceptable termination shapes:
 *   - WIFSIGNALED(status) && WTERMSIG(status) == SIGSEGV  (Linux-typical)
 *   - WIFEXITED(status)   && WEXITSTATUS(status) != 0     (custom-kernel
 *     handling that maps the fatal signal to an exit status)
 *
 * The bug under test is "SIG_IGN swallows the fault, child hangs or
 * returns 0 cleanly". Both of those are caught by this test.
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void)
{
    TEST_START("fault sig_ign");

    pid_t pid = fork();
    CHECK(pid >= 0, "fork crash child");
    if (pid < 0) {
        TEST_DONE();
    }

    if (pid == 0) {
        /* Child: tell the kernel we want to ignore SIGSEGV, then fault. */
        struct sigaction sa = {0};
        sa.sa_handler = SIG_IGN;
        sigemptyset(&sa.sa_mask);
        if (sigaction(SIGSEGV, &sa, NULL) != 0) {
            _exit(99);
        }
        /* Also block SIGSEGV — force-delivery has to bypass the
         * mask too, per the same Linux semantics. */
        sigset_t block;
        sigemptyset(&block);
        sigaddset(&block, SIGSEGV);
        if (sigprocmask(SIG_BLOCK, &block, NULL) != 0) {
            _exit(98);
        }

        volatile int *p = (volatile int *)0;
        *p = 42;

        /* If we got here, the fault was actually ignored — that's
         * the bug. Exit cleanly so the parent's WIFEXITED branch
         * catches it. */
        _exit(0);
    }

    int status = 0;
    pid_t got = waitpid(pid, &status, 0);
    CHECK(got == pid, "waitpid returned crash child");

    int by_signal = WIFSIGNALED(status) && WTERMSIG(status) == SIGSEGV;
    int by_exit_nonzero = WIFEXITED(status) && WEXITSTATUS(status) != 0;
    CHECK(by_signal || by_exit_nonzero,
          "sync SIGSEGV under SIG_IGN still terminates the process");
    /* Specifically reject the bug-shaped outcome where the kernel
     * let the child resume past the faulting instruction. */
    CHECK(!(WIFEXITED(status) && WEXITSTATUS(status) == 0),
          "sync SIGSEGV under SIG_IGN did not let the child exit cleanly");

    TEST_DONE();
}
