#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/ptrace.h>
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

int main(void)
{
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

    if (kill(child, SIGKILL) != 0) {
        return fail("kill child");
    }
    if (waitpid(child, &status, __WALL) != child || !WIFSIGNALED(status)
        || WTERMSIG(status) != SIGKILL) {
        printf("FAIL: expected SIGKILL through __WALL, status=%#x\n", status);
        return 1;
    }

    printf("DONE: 1 pass, 0 fail\n");
    return 0;
}
