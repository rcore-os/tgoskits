#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/wait.h>
#include <unistd.h>

static int fail(const char *msg)
{
    printf("FAIL: %s: errno=%d (%s)\n", msg, errno, strerror(errno));
    return 1;
}

static int read_tracer_pid(pid_t tracee)
{
    char path[64];
    snprintf(path, sizeof(path), "/proc/%ld/status", (long)tracee);

    FILE *file = fopen(path, "r");
    if (file == NULL) {
        return -1;
    }

    char line[256];
    while (fgets(line, sizeof(line), file) != NULL) {
        int tracer_pid = 0;
        if (sscanf(line, "TracerPid:\t%d", &tracer_pid) == 1) {
            fclose(file);
            return tracer_pid;
        }
    }

    fclose(file);
    errno = ENOENT;
    return -1;
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
    if (waitpid(child, &status, 0) != child) {
        return fail("waitpid child stop");
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGSTOP) {
        printf("FAIL: expected traced child SIGSTOP, status=%#x\n", status);
        return 1;
    }

    int tracer_pid = read_tracer_pid(child);
    if (tracer_pid < 0) {
        int saved_errno = errno;
        kill(child, SIGKILL);
        waitpid(child, &status, 0);
        errno = saved_errno;
        return fail("read TracerPid from /proc child status");
    }

    pid_t expected = getpid();
    if (tracer_pid != expected) {
        printf("FAIL: expected TracerPid %ld, got %d\n", (long)expected, tracer_pid);
        kill(child, SIGKILL);
        waitpid(child, &status, 0);
        return 1;
    }

    if (kill(child, SIGKILL) != 0) {
        return fail("kill child");
    }
    if (waitpid(child, &status, 0) != child || !WIFSIGNALED(status)
        || WTERMSIG(status) != SIGKILL) {
        printf("FAIL: expected child SIGKILL, status=%#x\n", status);
        return 1;
    }

    printf("DONE: 1 pass, 0 fail\n");
    return 0;
}
