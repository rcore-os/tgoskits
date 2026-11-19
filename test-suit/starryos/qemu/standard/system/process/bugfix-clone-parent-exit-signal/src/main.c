#define _GNU_SOURCE

#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef SYS_clone3
#define SYS_clone3 435
#endif

struct clone3_args {
    unsigned long long flags;
    unsigned long long pidfd;
    unsigned long long child_tid;
    unsigned long long parent_tid;
    unsigned long long exit_signal;
    unsigned long long stack;
    unsigned long long stack_size;
    unsigned long long tls;
    unsigned long long set_tid;
    unsigned long long set_tid_size;
    unsigned long long cgroup;
};

struct clone_report {
    pid_t child;
    int error;
};

static int write_report(int fd, const struct clone_report *report)
{
    const char *cursor = (const char *)report;
    size_t remaining = sizeof(*report);

    while (remaining > 0) {
        ssize_t written = write(fd, cursor, remaining);
        if (written < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        cursor += written;
        remaining -= (size_t)written;
    }
    return 0;
}

static int read_report(int fd, struct clone_report *report)
{
    char *cursor = (char *)report;
    size_t remaining = sizeof(*report);

    while (remaining > 0) {
        ssize_t received = read(fd, cursor, remaining);
        if (received == 0)
            return -1;
        if (received < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        cursor += received;
        remaining -= (size_t)received;
    }
    return 0;
}

int main(void)
{
    struct clone3_args clone3_args = {
        .flags = CLONE_PARENT,
        .exit_signal = SIGCHLD,
    };
    errno = 0;
    if (syscall(SYS_clone3, &clone3_args, sizeof(clone3_args)) != -1 ||
        errno != EINVAL) {
        printf("FAIL: clone3(CLONE_PARENT, exit_signal=SIGCHLD) must return "
               "EINVAL, errno=%d (%s)\n",
               errno, strerror(errno));
        return 1;
    }

    int report_pipe[2];
    if (pipe(report_pipe) != 0) {
        printf("FAIL: pipe errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    pid_t middle = fork();
    if (middle < 0) {
        printf("FAIL: fork errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }
    if (middle == 0) {
        close(report_pipe[0]);

        errno = 0;
        long child = syscall(SYS_clone, CLONE_PARENT | SIGCHLD, NULL, NULL,
                             NULL, 0);
        if (child == 0) {
            close(report_pipe[1]);
            _exit(23);
        }

        struct clone_report report = {
            .child = (pid_t)child,
            .error = child < 0 ? errno : 0,
        };
        int report_result = write_report(report_pipe[1], &report);
        close(report_pipe[1]);
        _exit(child < 0 || report_result != 0 ? 1 : 0);
    }

    close(report_pipe[1]);
    struct clone_report report;
    if (read_report(report_pipe[0], &report) != 0) {
        puts("FAIL: did not receive clone result");
        return 1;
    }
    close(report_pipe[0]);

    int middle_status;
    if (waitpid(middle, &middle_status, 0) != middle ||
        !WIFEXITED(middle_status) || WEXITSTATUS(middle_status) != 0) {
        printf("FAIL: clone(CLONE_PARENT|SIGCHLD) errno=%d (%s)\n",
               report.error, strerror(report.error));
        return 1;
    }

    int child_status;
    if (waitpid(report.child, &child_status, 0) != report.child) {
        printf("FAIL: grandparent cannot wait for CLONE_PARENT child errno=%d "
               "(%s)\n",
               errno, strerror(errno));
        return 1;
    }
    if (!WIFEXITED(child_status) || WEXITSTATUS(child_status) != 23) {
        puts("FAIL: CLONE_PARENT child exit status mismatch");
        return 1;
    }

    puts("CLONE_PARENT_EXIT_SIGNAL_PASSED");
    return 0;
}
