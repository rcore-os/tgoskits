/*
 * test-job-control-stop
 *
 * Regression test for POSIX job control. A `SIGSTOP` (or SIGTSTP/SIGTTIN/
 * SIGTTOU) must *suspend* the target process, not kill it; the parent's
 * waitpid(WUNTRACED) must observe WIFSTOPPED. A subsequent `SIGCONT` must
 * resume the process and waitpid(WCONTINUED) must observe WIFCONTINUED.
 * Finally SIGKILL must terminate even a stopped process.
 *
 * Before this fix StarryOS turned SignalOSAction::Stop into do_exit(1):
 * the child was *killed* by SIGSTOP, so the WUNTRACED check below would
 * have seen WIFEXITED/WIFSIGNALED instead of WIFSTOPPED and FAILed. That
 * broke shell job control and busybox `killall5 -STOP/-CONT`.
 */

#include "test_framework.h"
#include <sched.h>
#include <signal.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void)
{
    TEST_START("job control: SIGSTOP suspends, SIGCONT resumes");

    pid_t child = fork();
    CHECK(child >= 0, "fork");
    if (child < 0) {
        TEST_DONE();
    }

    if (child == 0) {
        /* Child: spin forever. It must keep "running" (be schedulable)
         * until stopped, and must be re-schedulable after SIGCONT. We rely
         * on the parent's kill+waitpid to drive the state transitions. */
        for (;;) {
            /* busy-yield so the parent gets CPU time on smp=1 */
            sched_yield();
        }
        _exit(0); /* unreachable */
    }

    int st;

    /* --- SIGSTOP must STOP, not kill --- */
    CHECK_RET(kill(child, SIGSTOP), 0, "kill(child, SIGSTOP)");

    /* waitpid(WUNTRACED) blocks until the child is stopped, then reports it.
     * If the old (buggy) behavior were present, the child would have exited
     * and we'd get WIFEXITED/WIFSIGNALED here instead. */
    pid_t w = waitpid(child, &st, WUNTRACED);
    CHECK_RET(w, child, "waitpid(WUNTRACED) returns child pid");
    CHECK(WIFSTOPPED(st), "child is STOPPED (WIFSTOPPED)");
    CHECK(!WIFEXITED(st), "child did NOT exit on SIGSTOP");
    CHECK(!WIFSIGNALED(st), "child was NOT killed by SIGSTOP");
    if (WIFSTOPPED(st)) {
        CHECK(WSTOPSIG(st) == SIGSTOP, "WSTOPSIG == SIGSTOP");
    }

    /* The child must still exist (a stopped process is not reaped). A
     * second kill(child, 0) probing existence should succeed. */
    CHECK_RET(kill(child, 0), 0, "stopped child still exists (kill 0)");

    /* --- SIGCONT must CONTINUE --- */
    CHECK_RET(kill(child, SIGCONT), 0, "kill(child, SIGCONT)");

    w = waitpid(child, &st, WCONTINUED);
    CHECK_RET(w, child, "waitpid(WCONTINUED) returns child pid");
    CHECK(WIFCONTINUED(st), "child reported CONTINUED (WIFCONTINUED)");

    /* --- SIGKILL must terminate even after a stop/continue cycle --- */
    CHECK_RET(kill(child, SIGKILL), 0, "kill(child, SIGKILL)");
    w = waitpid(child, &st, 0);
    CHECK_RET(w, child, "waitpid reaps killed child");
    CHECK(WIFSIGNALED(st), "child terminated by signal");
    if (WIFSIGNALED(st)) {
        CHECK(WTERMSIG(st) == SIGKILL, "WTERMSIG == SIGKILL");
    }

    TEST_DONE();
}
