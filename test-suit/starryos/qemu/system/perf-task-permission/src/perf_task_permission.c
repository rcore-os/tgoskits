/*
 * perf_task_permission.c -- Linux perf task-target credential boundary.
 *
 * A uid-1000 caller must not attach a software perf event to a dumpable
 * uid-2000 task merely because it knows the target TID. Linux v7.1 applies
 * perf_check_permission() and PTRACE_MODE_READ_REALCREDS even when
 * perf_event_paranoid permits unprivileged events.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define PERF_TYPE_SOFTWARE 1u
#define PERF_COUNT_SW_CPU_CLOCK 0ull

struct perf_event_attr {
    uint32_t type;
    uint32_t size;
    uint64_t config;
    uint64_t sample_period;
    uint64_t sample_type;
    uint64_t read_format;
    uint64_t flags;
};

#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
#endif

static long perf_event_open(struct perf_event_attr *attr, pid_t pid, int cpu) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, -1, 0ul);
}

static int fail(const char *reason) {
    printf("perf-task-permission FAILED: %s errno=%d\n", reason, errno);
    return 1;
}

int main(void) {
    int ready[2];
    int release[2];
    if (pipe(ready) != 0 || pipe(release) != 0) {
        return fail("pipe");
    }

    pid_t target = fork();
    if (target < 0) {
        return fail("fork");
    }
    if (target == 0) {
        close(ready[0]);
        close(release[1]);
        if (setresgid(2000, 2000, 2000) != 0 ||
            setresuid(2000, 2000, 2000) != 0 ||
            prctl(PR_SET_DUMPABLE, 1, 0, 0, 0) != 0) {
            _exit(2);
        }
        if (write(ready[1], "r", 1) != 1) {
            _exit(3);
        }
        char byte;
        if (read(release[0], &byte, 1) != 1) {
            _exit(4);
        }
        _exit(0);
    }

    close(ready[1]);
    close(release[0]);
    char byte;
    if (read(ready[0], &byte, 1) != 1) {
        return fail("target setup");
    }
    close(ready[0]);

    if (setresgid(1000, 1000, 1000) != 0 ||
        setresuid(1000, 1000, 1000) != 0) {
        return fail("caller credential setup");
    }

    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_SOFTWARE;
    attr.size = (uint32_t)sizeof(attr);
    attr.config = PERF_COUNT_SW_CPU_CLOCK;

    errno = 0;
    long fd = perf_event_open(&attr, target, -1);
    int open_errno = errno;
    if (fd >= 0) {
        close((int)fd);
    }

    attr.type = UINT32_MAX;
    errno = 0;
    long malformed_fd = perf_event_open(&attr, target, -1);
    int malformed_errno = errno;
    if (malformed_fd >= 0) {
        close((int)malformed_fd);
    }

    attr.type = PERF_TYPE_SOFTWARE;
    errno = 0;
    long missing_fd = perf_event_open(&attr, INT32_MAX, -2);
    int missing_errno = errno;
    if (missing_fd >= 0) {
        close((int)missing_fd);
    }

    if (write(release[1], "g", 1) != 1) {
        return fail("target release");
    }
    close(release[1]);
    int status = 0;
    if (waitpid(target, &status, 0) != target ||
        !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        return fail("target exit");
    }

    if (fd != -1 || open_errno != EACCES) {
        errno = open_errno;
        return fail("cross-credential perf_event_open did not return EACCES");
    }
    if (malformed_fd != -1 || malformed_errno != EINVAL) {
        errno = malformed_errno;
        return fail("malformed attr did not take precedence over permission");
    }
    if (missing_fd != -1 || missing_errno != ESRCH) {
        errno = missing_errno;
        return fail("missing TID did not take precedence over invalid CPU");
    }

    printf("STARRY_PERF_TASK_PERMISSION_OK\n");
    return 0;
}
