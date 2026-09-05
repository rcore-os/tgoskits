#include "test.h"

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <unistd.h>

/* Upper bound for the fill loop: the probe stops as soon as the pipe is full. */
#define PIPE_FILL_LIMIT 4096

static struct epoll_event make_event(uint32_t events, int fd)
{
    struct epoll_event event;

    memset(&event, 0, sizeof(event));
    event.events = events;
    event.data.fd = fd;
    return event;
}

static int expect_errno_int(int ret, int err, char *reason, size_t reason_len, const char *name)
{
    if (ret != -1 || errno != err) {
        test_fail(reason, reason_len, "%s returned %d errno=%d expected errno=%d", name, ret,
                  errno, err);
        return -1;
    }
    return 0;
}

static int expect_errno_ssize(ssize_t ret, int err, char *reason, size_t reason_len,
                              const char *name)
{
    if (ret != -1 || errno != err) {
        test_fail(reason, reason_len, "%s returned %ld errno=%d expected errno=%d", name,
                  (long)ret, errno, err);
        return -1;
    }
    return 0;
}

static int expect_no_event(int epfd, char *reason, size_t reason_len)
{
    struct epoll_event event;

    memset(&event, 0, sizeof(event));
    if (epoll_wait(epfd, &event, 1, 0) != 0) {
        test_fail(reason, reason_len, "epoll_wait expected no event");
        return -1;
    }
    return 0;
}

static int expect_one_event(int epfd, uint32_t events, int fd, char *reason, size_t reason_len)
{
    struct epoll_event event;
    int ret;

    memset(&event, 0, sizeof(event));
    ret = epoll_wait(epfd, &event, 1, 0);
    if (ret != 1 || event.data.fd != fd || (event.events & events) != events) {
        test_fail(reason, reason_len, "epoll_wait ret=%d fd=%d events=0x%x expected fd=%d events=0x%x",
                  ret, event.data.fd, event.events, fd, events);
        return -1;
    }
    return 0;
}

int arceos_c_test_pipe(char *reason, size_t reason_len)
{
    int fd[2];
    char buf[32] = {0};
    const char *message = "pipe message";

    CHECK_RET(pipe(fd), 0);
    CHECK_RET(write(fd[1], message, strlen(message) + 1), (ssize_t)(strlen(message) + 1));
    CHECK_RET(read(fd[0], buf, sizeof(buf)), (ssize_t)(strlen(message) + 1));
    CHECK_RET(strcmp(buf, message), 0);
    CHECK_RET(close(fd[1]), 0);
    CHECK_RET(read(fd[0], buf, sizeof(buf)), 0);
    CHECK_RET(close(fd[0]), 0);
    puts("pipe: pipe APIs OK");
    return 0;
}

static int test_epoll_arguments(char *reason, size_t reason_len)
{
    struct epoll_event event = make_event(EPOLLIN, 0);
    char byte = 0;
    int epfd;
    int pipefd[2];

    errno = 0;
    if (expect_errno_int(epoll_create(0), EINVAL, reason, reason_len, "epoll_create(0)") != 0) {
        return -1;
    }

    errno = 0;
    if (expect_errno_int(epoll_create1(EPOLL_NONBLOCK), EINVAL, reason, reason_len,
                         "epoll_create1(EPOLL_NONBLOCK)") != 0) {
        return -1;
    }

    epfd = epoll_create1(EPOLL_CLOEXEC);
    CHECK_TRUE(epfd >= 0);

    errno = 0;
    if (expect_errno_int(epoll_wait(epfd, &event, 0, 0), EINVAL, reason, reason_len,
                         "epoll_wait maxevents=0") != 0) {
        return -1;
    }

    errno = 0;
    if (expect_errno_int(epoll_wait(epfd, NULL, 1, 0), EFAULT, reason, reason_len,
                         "epoll_wait NULL") != 0) {
        return -1;
    }

    errno = 0;
    if (expect_errno_ssize(read(epfd, &byte, sizeof(byte)), EINVAL, reason, reason_len,
                           "read epfd") != 0) {
        return -1;
    }

    errno = 0;
    if (expect_errno_ssize(write(epfd, "x", 1), EINVAL, reason, reason_len, "write epfd") != 0) {
        return -1;
    }

    CHECK_RET(pipe(pipefd), 0);

    errno = 0;
    if (expect_errno_int(epoll_ctl(epfd, EPOLL_CTL_ADD, pipefd[0], NULL), EFAULT, reason,
                         reason_len, "epoll_ctl NULL event") != 0) {
        return -1;
    }

    errno = 0;
    if (expect_errno_int(epoll_ctl(epfd, EPOLL_CTL_ADD, epfd, &event), EINVAL, reason, reason_len,
                         "epoll_ctl self add") != 0) {
        return -1;
    }

    close(pipefd[0]);
    close(pipefd[1]);
    close(epfd);
    return 0;
}

static int test_level_triggered_epoll(int epfd, int write_fd, char *reason, size_t reason_len)
{
    struct epoll_event event = make_event(EPOLLOUT, write_fd);

    CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, write_fd, &event), 0);
    if (expect_one_event(epfd, EPOLLOUT, write_fd, reason, reason_len) != 0) {
        return -1;
    }
    if (expect_one_event(epfd, EPOLLOUT, write_fd, reason, reason_len) != 0) {
        return -1;
    }
    CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_DEL, write_fd, NULL), 0);
    return expect_no_event(epfd, reason, reason_len);
}

static int test_edge_triggered_epoll(int epfd, int read_fd, int write_fd, char *reason,
                                     size_t reason_len)
{
    char buf[4];
    struct epoll_event event = make_event(EPOLLIN | EPOLLET, read_fd);

    CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, read_fd, &event), 0);
    if (expect_no_event(epfd, reason, reason_len) != 0) {
        return -1;
    }
    CHECK_RET(write(write_fd, "abc", 3), 3);
    if (expect_one_event(epfd, EPOLLIN, read_fd, reason, reason_len) != 0) {
        return -1;
    }
    if (expect_no_event(epfd, reason, reason_len) != 0) {
        return -1;
    }
    CHECK_RET(read(read_fd, buf, 1), 1);
    if (expect_no_event(epfd, reason, reason_len) != 0) {
        return -1;
    }
    CHECK_RET(read(read_fd, buf, 2), 2);
    CHECK_RET(write(write_fd, "d", 1), 1);
    if (expect_one_event(epfd, EPOLLIN, read_fd, reason, reason_len) != 0) {
        return -1;
    }
    CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_DEL, read_fd, NULL), 0);
    CHECK_RET(read(read_fd, buf, 1), 1);
    return expect_no_event(epfd, reason, reason_len);
}

/*
 * A write that overflows the pipe blocks until a reader drains it, so the fill
 * loop probes writability with a level-triggered `EPOLLOUT` watch on a throwaway
 * epoll instance instead of assuming a pipe capacity.
 */
static int fill_pipe_until_full(int probe_epfd, int write_fd, char *reason, size_t reason_len)
{
    struct epoll_event event = make_event(EPOLLOUT, write_fd);
    struct epoll_event ready;
    int i;

    CHECK_RET(epoll_ctl(probe_epfd, EPOLL_CTL_ADD, write_fd, &event), 0);
    for (i = 0; i < PIPE_FILL_LIMIT; i++) {
        memset(&ready, 0, sizeof(ready));
        if (epoll_wait(probe_epfd, &ready, 1, 0) == 0) {
            CHECK_RET(epoll_ctl(probe_epfd, EPOLL_CTL_DEL, write_fd, NULL), 0);
            return 0;
        }
        CHECK_RET(write(write_fd, "x", 1), 1);
    }
    test_fail(reason, reason_len, "pipe still writable after %d bytes", PIPE_FILL_LIMIT);
    return -1;
}

/*
 * An edge-triggered `EPOLLOUT` watch on the write end must report the
 * Full -> Normal transition even when the pipe is filled and drained between two
 * `epoll_wait` calls: no wait observes the Full state, so a delivery rule that
 * only compares the previously sampled writability drops the edge and leaves a
 * writer waiting for a wake that never comes.
 */
static int test_edge_triggered_epollout(int epfd, int read_fd, int write_fd, char *reason,
                                        size_t reason_len)
{
    char buf[1];
    struct epoll_event event = make_event(EPOLLOUT | EPOLLET, write_fd);
    int probe_epfd;

    CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, write_fd, &event), 0);
    if (expect_one_event(epfd, EPOLLOUT, write_fd, reason, reason_len) != 0) {
        return -1;
    }
    if (expect_no_event(epfd, reason, reason_len) != 0) {
        return -1;
    }

    probe_epfd = epoll_create(1);
    CHECK_TRUE(probe_epfd >= 0);
    if (fill_pipe_until_full(probe_epfd, write_fd, reason, reason_len) != 0) {
        close(probe_epfd);
        return -1;
    }
    close(probe_epfd);

    /* Full -> Normal with no wait in between: the writable edge must survive. */
    CHECK_RET(read(read_fd, buf, 1), 1);
    if (expect_one_event(epfd, EPOLLOUT, write_fd, reason, reason_len) != 0) {
        return -1;
    }
    if (expect_no_event(epfd, reason, reason_len) != 0) {
        return -1;
    }
    CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_DEL, write_fd, NULL), 0);
    return 0;
}

static int test_oneshot_epoll(int epfd, int read_fd, int write_fd, char *reason,
                              size_t reason_len)
{
    char buf[1];
    struct epoll_event event = make_event(EPOLLIN | EPOLLONESHOT, read_fd);

    CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, read_fd, &event), 0);
    CHECK_RET(write(write_fd, "z", 1), 1);
    if (expect_one_event(epfd, EPOLLIN, read_fd, reason, reason_len) != 0) {
        return -1;
    }
    if (expect_no_event(epfd, reason, reason_len) != 0) {
        return -1;
    }
    CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_MOD, read_fd, &event), 0);
    if (expect_one_event(epfd, EPOLLIN, read_fd, reason, reason_len) != 0) {
        return -1;
    }
    CHECK_RET(read(read_fd, buf, 1), 1);
    CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_DEL, read_fd, NULL), 0);
    return expect_no_event(epfd, reason, reason_len);
}

static int test_registered_fd_close(char *reason, size_t reason_len)
{
    char buf[1];
    int epfd;
    int pipefd[2];
    struct epoll_event event;

    epfd = epoll_create(1);
    CHECK_TRUE(epfd >= 0);
    CHECK_RET(pipe(pipefd), 0);

    event = make_event(EPOLLOUT, pipefd[1]);
    CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, pipefd[1], &event), 0);
    if (expect_one_event(epfd, EPOLLOUT, pipefd[1], reason, reason_len) != 0) {
        return -1;
    }
    CHECK_RET(close(pipefd[1]), 0);
    if (expect_no_event(epfd, reason, reason_len) != 0) {
        return -1;
    }
    CHECK_RET(read(pipefd[0], buf, sizeof(buf)), 0);
    close(pipefd[0]);
    close(epfd);
    return 0;
}

int arceos_c_test_epoll(char *reason, size_t reason_len)
{
    int epfd;
    int pipefd[2];

    if (test_epoll_arguments(reason, reason_len) != 0) {
        return -1;
    }

    epfd = epoll_create(1);
    CHECK_TRUE(epfd >= 0);
    CHECK_RET(pipe(pipefd), 0);

    if (test_level_triggered_epoll(epfd, pipefd[1], reason, reason_len) != 0 ||
        test_edge_triggered_epoll(epfd, pipefd[0], pipefd[1], reason, reason_len) != 0 ||
        test_oneshot_epoll(epfd, pipefd[0], pipefd[1], reason, reason_len) != 0 ||
        test_edge_triggered_epollout(epfd, pipefd[0], pipefd[1], reason, reason_len) != 0 ||
        test_registered_fd_close(reason, reason_len) != 0) {
        close(pipefd[0]);
        close(pipefd[1]);
        close(epfd);
        return -1;
    }

    close(pipefd[0]);
    close(pipefd[1]);
    close(epfd);
    puts("epoll: epoll APIs OK");
    return 0;
}
