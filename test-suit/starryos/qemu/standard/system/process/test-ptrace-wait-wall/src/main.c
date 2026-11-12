#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <sched.h>
#include <stdio.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef __WALL
#define __WALL 0x40000000
#endif

static int fail(const char *msg)
{
    printf("FAIL: %s: errno=%d (%s)\n", msg, errno, strerror(errno));
    return 1;
}

static void ignore_usr1(int signo)
{
    (void)signo;
}

int main(void)
{
    cpu_set_t original, one;
    struct sched_param saved, fifo = {.sched_priority = 1};
    int policy = syscall(SYS_sched_getscheduler, 0);
    if (policy < 0 || syscall(SYS_sched_getparam, 0, &saved) != 0
        || syscall(SYS_sched_getaffinity, 0, sizeof(original), &original) < 0) {
        return fail("read tracer scheduling state");
    }
    CPU_ZERO(&one);
    for (int cpu = 0; cpu < CPU_SETSIZE; cpu++) {
        if (CPU_ISSET(cpu, &original)) {
            CPU_SET(cpu, &one);
            break;
        }
    }
    if (syscall(SYS_sched_setaffinity, 0, sizeof(one), &one) != 0
        || syscall(SYS_sched_setscheduler, 0, SCHED_FIFO, &fifo) != 0) {
        return fail("establish same-CPU FIFO handoff");
    }
    struct sigaction action = {.sa_handler = ignore_usr1};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGUSR1, &action, NULL) != 0) {
        return fail("install nonfatal signal handler");
    }
    pid_t child = fork();
    if (child < 0) {
        return fail("fork");
    }

    if (child == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(101);
        }
        if (kill(getpid(), SIGSTOP) != 0) {
            _exit(102);
        }
        _exit(0);
    }

    int status = 0;
    if (waitpid(child, &status, __WALL) != child) {
        return fail("waitpid __WALL ptrace stop");
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGSTOP) {
        printf("FAIL: expected ptrace SIGSTOP through __WALL, status=%#x\n", status);
        return 1;
    }

    /* Both tasks inherit one CPU and equal FIFO priority. waitpid observes
     * the stop after the tracee has switched out. A yield after the signal
     * lets the now-runnable tracee consume the interruption and park again
     * before this tracer resumes; no sleep or sampling window is involved. */
    if (kill(child, SIGUSR1) != 0 || syscall(SYS_sched_yield) != 0) {
        return fail("interrupt stopped tracee and hand off CPU");
    }
    siginfo_t info;
    memset(&info, 0, sizeof(info));
    if (ptrace(PTRACE_GETSIGINFO, child, NULL, &info) != 0) {
        return fail("read original ptrace stop after nonfatal signal");
    }
    if (info.si_signo != SIGSTOP) {
        printf("FAIL: nonfatal signal replaced ptrace stop: got %d, expected %d\n",
               info.si_signo, SIGSTOP);
        return 1;
    }
    if (waitpid(child, &status, __WALL | WNOHANG) != 0) {
        return fail("nonfatal signal must not publish a second stop");
    }

    if (kill(child, SIGKILL) != 0) {
        return fail("kill child");
    }
    if (waitpid(child, &status, __WALL) != child || !WIFSIGNALED(status)
        || WTERMSIG(status) != SIGKILL) {
        printf("FAIL: expected SIGKILL through __WALL, status=%#x\n", status);
        return 1;
    }

    if (syscall(SYS_sched_setscheduler, 0, policy, &saved) != 0
        || syscall(SYS_sched_setaffinity, 0, sizeof(original), &original) < 0) {
        return fail("restore tracer scheduling state");
    }
    printf("DONE: ptrace stop survives nonfatal interruption; SIGKILL releases it\n");
    return 0;
}
