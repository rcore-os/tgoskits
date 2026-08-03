#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef PTRACE_SEIZE
#define PTRACE_SEIZE 0x4206
#endif
#ifndef PTRACE_INTERRUPT
#define PTRACE_INTERRUPT 0x4207
#endif
#ifndef PTRACE_EVENT_STOP
#define PTRACE_EVENT_STOP 128
#endif
#ifndef PTRACE_O_TRACESYSGOOD
#define PTRACE_O_TRACESYSGOOD 0x00000001
#endif
#ifndef __WALL
#define __WALL 0x40000000
#endif

enum {
    WAIT_TIMEOUT_MS = 3000,
    WAIT_POLL_INTERVAL_MS = 10,
};

static int fail(const char *message)
{
    printf("FAIL: %s: errno=%d (%s)\n", message, errno, strerror(errno));
    return 1;
}

static pid_t wait_for_tracee_event(pid_t pid, int *status)
{
    struct timespec delay = {
        .tv_sec = 0,
        .tv_nsec = WAIT_POLL_INTERVAL_MS * 1000 * 1000,
    };

    for (int waited_ms = 0; waited_ms < WAIT_TIMEOUT_MS; waited_ms += WAIT_POLL_INTERVAL_MS) {
        pid_t waited = waitpid(pid, status, __WALL | WNOHANG);
        if (waited != 0) {
            return waited;
        }
        (void)nanosleep(&delay, NULL);
    }

    errno = ETIMEDOUT;
    return 0;
}

static int expect_ptrace_stop(pid_t pid, int expected_signal, int expected_event,
                              const char *description)
{
    int status = 0;
    pid_t waited = wait_for_tracee_event(pid, &status);
    if (waited != pid) {
        return fail(description);
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != expected_signal
        || ((unsigned int)status >> 16) != (unsigned int)expected_event) {
        printf("FAIL: %s: status=%#x signal=%d event=%u\n", description, status,
               WIFSTOPPED(status) ? WSTOPSIG(status) : -1, (unsigned int)status >> 16);
        return 1;
    }
    return 0;
}

static int expect_getsiginfo_signal(pid_t pid, int expected_signal, const char *description)
{
    siginfo_t siginfo;
    memset(&siginfo, 0, sizeof(siginfo));
    errno = 0;
    long result = ptrace(PTRACE_GETSIGINFO, pid, NULL, &siginfo);
    if (result != 0 || siginfo.si_signo != expected_signal) {
        printf("FAIL: %s: result=%ld signo=%d errno=%d (%s)\n", description,
               result, siginfo.si_signo, errno, strerror(errno));
        return 1;
    }
    return 0;
}

static void kill_tracee(pid_t pid)
{
    int status = 0;
    (void)kill(pid, SIGKILL);
    (void)wait_for_tracee_event(pid, &status);
}

int main(void)
{
    printf("PTRACE_SEIZE syscall-stop regression\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork tracee");
    }
    if (pid == 0) {
        for (;;) {
            (void)syscall(SYS_getpid);
        }
    }

    if (ptrace(PTRACE_SEIZE, pid, NULL, (void *)(uintptr_t)PTRACE_O_TRACESYSGOOD) != 0) {
        kill_tracee(pid);
        return fail("PTRACE_SEIZE with PTRACE_O_TRACESYSGOOD");
    }
    if (ptrace(PTRACE_INTERRUPT, pid, NULL, NULL) != 0) {
        kill_tracee(pid);
        return fail("PTRACE_INTERRUPT");
    }
    if (expect_ptrace_stop(pid, SIGTRAP, PTRACE_EVENT_STOP,
                            "PTRACE_INTERRUPT reports PTRACE_EVENT_STOP")
        != 0) {
        kill_tracee(pid);
        return 1;
    }
    if (expect_getsiginfo_signal(pid, SIGTRAP,
                                 "PTRACE_EVENT_STOP PTRACE_GETSIGINFO returns SIGTRAP")
        != 0) {
        kill_tracee(pid);
        return 1;
    }

    if (ptrace(PTRACE_SYSCALL, pid, NULL, (void *)(uintptr_t)SIGCONT) != 0) {
        kill_tracee(pid);
        return fail("PTRACE_SYSCALL resumes PTRACE_EVENT_STOP with SIGCONT");
    }
    if (expect_ptrace_stop(pid, SIGTRAP | 0x80, 0,
                            "PTRACE_SYSCALL reports a TRACESYSGOOD syscall stop")
        != 0) {
        kill_tracee(pid);
        return 1;
    }
    if (expect_getsiginfo_signal(pid, SIGTRAP,
                                 "syscall stop PTRACE_GETSIGINFO returns SIGTRAP")
        != 0) {
        kill_tracee(pid);
        return 1;
    }

    if (ptrace(PTRACE_SYSCALL, pid, NULL, (void *)(uintptr_t)SIGSTOP) != 0) {
        kill_tracee(pid);
        return fail("PTRACE_SYSCALL resumes syscall stop with SIGSTOP");
    }
    if (expect_ptrace_stop(pid, SIGTRAP | 0x80, 0,
                            "PTRACE_SYSCALL preserves the syscall-exit stop before SIGSTOP")
        != 0) {
        kill_tracee(pid);
        return 1;
    }
    if (ptrace(PTRACE_SYSCALL, pid, NULL, NULL) != 0) {
        kill_tracee(pid);
        return fail("PTRACE_SYSCALL resumes syscall-exit stop");
    }
    if (expect_ptrace_stop(pid, SIGSTOP, 0,
                            "PTRACE_SYSCALL signal injection reports SIGSTOP delivery stop")
        != 0) {
        kill_tracee(pid);
        return 1;
    }

    printf("PASS: PTRACE_SEIZE interrupt and syscall signal-stop sequence\n");
    kill_tracee(pid);
    return 0;
}
