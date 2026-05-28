#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef PTRACE_GETSIGINFO
#define PTRACE_GETSIGINFO 0x4202
#endif
#ifndef PTRACE_GETREGSET
#define PTRACE_GETREGSET 0x4204
#endif
#ifndef NT_PRSTATUS
#define NT_PRSTATUS 1
#endif

static int fail(const char *msg)
{
    printf("FAIL: %s: errno=%d (%s)\n", msg, errno, strerror(errno));
    return 1;
}

static int test_exec_stop(void)
{
    printf("test 1: execve SIGTRAP stop\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        execl("/usr/bin/test-ptrace-exec-stop", "test-ptrace-exec-stop", "--after-exec", NULL);
        _exit(101);
    }

    int status = 0;
    pid_t got = waitpid(pid, &status, WUNTRACED);
    if (got != pid) {
        return fail("waitpid exec child");
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGTRAP) {
        printf("FAIL: expected SIGTRAP after exec, status=%#x\n", status);
        return 1;
    }
    printf("  ok: child stopped with SIGTRAP after exec\n");

    siginfo_t si;
    memset(&si, 0, sizeof(si));
    if (ptrace(PTRACE_GETSIGINFO, pid, NULL, &si) != 0) {
        return fail("ptrace getsiginfo");
    }
    if (si.si_signo != SIGTRAP) {
        printf("FAIL: getsiginfo si_signo=%d, expected SIGTRAP(%d)\n",
               si.si_signo, SIGTRAP);
        return 1;
    }
    printf("  ok: GETSIGINFO returns si_signo=SIGTRAP\n");

    if (ptrace(PTRACE_DETACH, pid, NULL, NULL) != 0) {
        return fail("ptrace detach");
    }
    printf("  ok: PTRACE_DETACH succeeded\n");

    got = waitpid(pid, &status, 0);
    if (got != pid) {
        return fail("waitpid after detach");
    }
    printf("  ok: child reaped after detach, status=%#x\n", status);
    return 0;
}

static int test_detach_with_signal(void)
{
    printf("test 2: PTRACE_DETACH with signal injection\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);
        _exit(0);
    }

    int status = 0;
    pid_t got = waitpid(pid, &status, WUNTRACED);
    if (got != pid) {
        return fail("waitpid sigstop child");
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGSTOP) {
        printf("FAIL: expected SIGSTOP, status=%#x\n", status);
        return 1;
    }

    if (ptrace(PTRACE_DETACH, pid, NULL, (void *)(long)SIGCONT) != 0) {
        return fail("ptrace detach with SIGCONT");
    }
    printf("  ok: PTRACE_DETACH(pid, SIGCONT) succeeded\n");

    got = waitpid(pid, &status, 0);
    if (got != pid) {
        return fail("waitpid after detach+signal");
    }
    printf("  ok: child reaped after detach+signal, status=%#x\n", status);
    return 0;
}

static int test_cont_suppress_signal(void)
{
    printf("test 3: PTRACE_CONT suppresses signal\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGUSR1);
        _exit(42);
    }

    int status = 0;
    pid_t got = waitpid(pid, &status, WUNTRACED);
    if (got != pid) {
        return fail("waitpid signal child");
    }
    if (!WIFSTOPPED(status)) {
        printf("FAIL: expected stopped child, status=%#x\n", status);
        return 1;
    }
    printf("  ok: child stopped with signal %d\n", WSTOPSIG(status));

    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("ptrace cont (suppress signal)");
    }
    printf("  ok: PTRACE_CONT with signal=0 suppressed the signal\n");

    got = waitpid(pid, &status, 0);
    if (got != pid) {
        return fail("waitpid after cont");
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 42) {
        printf("FAIL: expected exit 42, status=%#x\n", status);
        return 1;
    }
    printf("  ok: child exited with 42 (signal suppressed)\n");
    return 0;
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "--after-exec") == 0) {
        return 0;
    }

    int pass = 0;
    int fail_count = 0;

    if (test_exec_stop() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_detach_with_signal() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_cont_suppress_signal() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    printf("DONE: %d pass, %d fail\n", pass, fail_count);
    return fail_count > 0 ? 1 : 0;
}
