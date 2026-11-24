#include "test_framework.h"

#include <stdint.h>
#include <sys/epoll.h>
#include <unistd.h>

static void close_pipe(int pipefd[2])
{
    if (pipefd[0] >= 0) {
        close(pipefd[0]);
    }
    if (pipefd[1] >= 0) {
        close(pipefd[1]);
    }
}

int main(void)
{
    /*
     * 回归目标：
     * 1) Linux 合法的 EPOLLEXCLUSIVE ADD 必须被接受；
     * 2) Linux 明确非法的 EPOLLEXCLUSIVE 组合必须返回 EINVAL。
     * 3) StarryOS 目前不放行 EPOLLWAKEUP，因此相关组合仍应返回 EINVAL。
     */
    TEST_START("epoll_ctl: EPOLLEXCLUSIVE Linux ABI checks");

    int epfd = epoll_create1(EPOLL_CLOEXEC);
    CHECK(epfd >= 0, "epoll_create1 succeeds");
    if (epfd < 0) {
        TEST_DONE();
    }

    int ok_pipe[2] = {-1, -1};
    CHECK_RET(pipe(ok_pipe), 0, "pipe for EPOLLEXCLUSIVE positive case");
    if (ok_pipe[0] >= 0) {
        struct epoll_event ev = {
            .events = EPOLLIN | EPOLLEXCLUSIVE,
            .data.fd = ok_pipe[0],
        };
        CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, ok_pipe[0], &ev), 0,
                  "epoll_ctl ADD with EPOLLEXCLUSIVE returns 0");

        CHECK_ERR(epoll_ctl(epfd, EPOLL_CTL_MOD, ok_pipe[0], &ev), EINVAL,
                  "epoll_ctl MOD with EPOLLEXCLUSIVE returns EINVAL");

        ev.events = EPOLLIN;
        CHECK_ERR(epoll_ctl(epfd, EPOLL_CTL_MOD, ok_pipe[0], &ev), EINVAL,
                  "epoll_ctl MOD after exclusive ADD returns EINVAL");
    }

    int oneshot_pipe[2] = {-1, -1};
    CHECK_RET(pipe(oneshot_pipe), 0, "pipe for EPOLLONESHOT negative case");
    if (oneshot_pipe[0] >= 0) {
        struct epoll_event ev = {
            .events = EPOLLIN | EPOLLONESHOT | EPOLLEXCLUSIVE,
            .data.fd = oneshot_pipe[0],
        };
        CHECK_ERR(epoll_ctl(epfd, EPOLL_CTL_ADD, oneshot_pipe[0], &ev), EINVAL,
                  "epoll_ctl ADD rejects EPOLLONESHOT with EPOLLEXCLUSIVE");
    }

    int rdhup_pipe[2] = {-1, -1};
    CHECK_RET(pipe(rdhup_pipe), 0, "pipe for EPOLLRDHUP negative case");
    if (rdhup_pipe[0] >= 0) {
        struct epoll_event ev = {
            .events = EPOLLIN | EPOLLRDHUP | EPOLLEXCLUSIVE,
            .data.fd = rdhup_pipe[0],
        };
        CHECK_ERR(epoll_ctl(epfd, EPOLL_CTL_ADD, rdhup_pipe[0], &ev), EINVAL,
                  "epoll_ctl ADD rejects EPOLLRDHUP with EPOLLEXCLUSIVE");
    }

    int pri_pipe[2] = {-1, -1};
    CHECK_RET(pipe(pri_pipe), 0, "pipe for EPOLLPRI negative case");
    if (pri_pipe[0] >= 0) {
        struct epoll_event ev = {
            .events = EPOLLIN | EPOLLPRI | EPOLLEXCLUSIVE,
            .data.fd = pri_pipe[0],
        };
        CHECK_ERR(epoll_ctl(epfd, EPOLL_CTL_ADD, pri_pipe[0], &ev), EINVAL,
                  "epoll_ctl ADD rejects EPOLLPRI with EPOLLEXCLUSIVE");
    }

    int wake_pipe[2] = {-1, -1};
    CHECK_RET(pipe(wake_pipe), 0, "pipe for EPOLLWAKEUP negative case");
    if (wake_pipe[0] >= 0) {
        struct epoll_event ev = {
            .events = EPOLLIN | EPOLLWAKEUP | EPOLLEXCLUSIVE,
            .data.fd = wake_pipe[0],
        };
        CHECK_ERR(epoll_ctl(epfd, EPOLL_CTL_ADD, wake_pipe[0], &ev), EINVAL,
                  "epoll_ctl ADD rejects EPOLLWAKEUP with EPOLLEXCLUSIVE on StarryOS");
    }

    struct epoll_event ev = {
        .events = EPOLLIN | EPOLLEXCLUSIVE,
        .data.fd = epfd,
    };
    CHECK_ERR(epoll_ctl(epfd, EPOLL_CTL_ADD, epfd, &ev), EINVAL,
              "epoll_ctl ADD rejects epoll target with EPOLLEXCLUSIVE");

    close_pipe(ok_pipe);
    close_pipe(oneshot_pipe);
    close_pipe(rdhup_pipe);
    close_pipe(pri_pipe);
    close_pipe(wake_pipe);
    close(epfd);

    TEST_DONE();
}
