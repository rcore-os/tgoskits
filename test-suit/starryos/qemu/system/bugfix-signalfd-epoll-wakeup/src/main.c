#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/signalfd.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int passed;
static int failed;

static void expect_true(int condition, const char *name)
{
    if (condition) {
        printf("PASS: %s\n", name);
        passed++;
        return;
    }
    printf("FAIL: %s: errno=%d (%s)\n", name, errno, strerror(errno));
    failed++;
}

int main(void)
{
    printf("=== bugfix-signalfd-epoll-wakeup ===\n");

    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGCHLD);
    expect_true(sigprocmask(SIG_BLOCK, &mask, NULL) == 0, "block SIGCHLD");

    int signal_fd = signalfd(-1, &mask, SFD_CLOEXEC | SFD_NONBLOCK);
    expect_true(signal_fd >= 0, "create nonblocking signalfd");

    int epoll_fd = epoll_create1(EPOLL_CLOEXEC);
    expect_true(epoll_fd >= 0, "create epoll");

    struct epoll_event interest = {
        .events = EPOLLIN,
        .data.fd = signal_fd,
    };
    expect_true(signal_fd >= 0 && epoll_fd >= 0 &&
                    epoll_ctl(epoll_fd, EPOLL_CTL_ADD, signal_fd,
                              &interest) == 0,
                "register signalfd with epoll");

    pid_t child = fork();
    expect_true(child >= 0, "fork delayed child");
    if (child == 0) {
        usleep(200000);
        _exit(7);
    }

    if (child > 0 && signal_fd >= 0 && epoll_fd >= 0) {
        struct epoll_event event = {0};
        errno = 0;
        int ready = epoll_wait(epoll_fd, &event, 1, 2000);
        expect_true(ready == 1 && event.data.fd == signal_fd &&
                        (event.events & EPOLLIN) != 0,
                    "epoll wakes when blocked SIGCHLD reaches signalfd");

        struct signalfd_siginfo signal_info = {0};
        ssize_t length = read(signal_fd, &signal_info, sizeof(signal_info));
        expect_true(length == (ssize_t)sizeof(signal_info),
                    "read SIGCHLD from signalfd");
        expect_true(length == (ssize_t)sizeof(signal_info) &&
                        signal_info.ssi_signo == SIGCHLD &&
                        signal_info.ssi_pid == (uint32_t)child,
                    "signalfd reports exiting child");

        siginfo_t child_info = {0};
        expect_true(waitid(P_ALL, 0, &child_info,
                           WEXITED | WNOHANG | WNOWAIT) == 0 &&
                        child_info.si_pid == child &&
                        child_info.si_code == CLD_EXITED &&
                        child_info.si_status == 7,
                    "waitid WNOWAIT observes child after signalfd wake");

        memset(&child_info, 0, sizeof(child_info));
        expect_true(waitid(P_PID, (id_t)child, &child_info, WEXITED) == 0 &&
                        child_info.si_pid == child &&
                        child_info.si_code == CLD_EXITED &&
                        child_info.si_status == 7,
                    "waitid P_PID reaps observed child");
    } else if (child > 0) {
        waitpid(child, NULL, 0);
    }

    if (epoll_fd >= 0) {
        close(epoll_fd);
    }
    if (signal_fd >= 0) {
        close(signal_fd);
    }

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("STARRY_SIGNALFD_EPOLL_WAKEUP_PASSED\n");
        printf("STARRY_GROUPED_TEST_PASSED: bugfix-signalfd-epoll-wakeup\n");
        return EXIT_SUCCESS;
    }
    printf("STARRY_GROUPED_TEST_FAILED: bugfix-signalfd-epoll-wakeup\n");
    return EXIT_FAILURE;
}
