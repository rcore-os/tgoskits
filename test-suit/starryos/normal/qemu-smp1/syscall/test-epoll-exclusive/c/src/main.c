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
     * 1) EPOLLIN | EPOLLEXCLUSIVE 必须被接受（返回 0）；
     * 2) 未知 epoll 事件位仍应按 Linux 语义返回 EINVAL。
     */
    TEST_START("epoll_ctl: EPOLLEXCLUSIVE accepted and unknown flag rejected");

    int epfd = epoll_create1(EPOLL_CLOEXEC);
    CHECK(epfd >= 0, "epoll_create1 succeeds");
    if (epfd < 0) {
        TEST_DONE();
    }

    int ok_pipe[2] = {-1, -1};
    CHECK_RET(pipe(ok_pipe), 0, "pipe for EPOLLEXCLUSIVE positive case");
    if (ok_pipe[0] >= 0) {
        /* 正例：验证 EPOLLEXCLUSIVE 兼容点。 */
        struct epoll_event ev = {
            .events = EPOLLIN | EPOLLEXCLUSIVE,
            .data.fd = ok_pipe[0],
        };
        CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, ok_pipe[0], &ev), 0,
                  "epoll_ctl ADD with EPOLLEXCLUSIVE returns 0");
    }

    int bad_pipe[2] = {-1, -1};
    CHECK_RET(pipe(bad_pipe), 0, "pipe for unknown flag negative case");
    if (bad_pipe[0] >= 0) {
        /*
         * 反例：注入一个未知事件位，确保 flag 校验没有被放宽。
         * 这里选用 1<<27（基于当前 Linux ABI 定义下的未知位）
         * 来触发 EINVAL。若未来该位被定义为合法 flag，需要同步调整。
         */
        struct epoll_event ev = {
            .events = EPOLLIN | ((uint32_t)1u << 27),
            .data.fd = bad_pipe[0],
        };
        CHECK_ERR(epoll_ctl(epfd, EPOLL_CTL_ADD, bad_pipe[0], &ev), EINVAL,
                  "epoll_ctl ADD rejects unknown epoll flag with EINVAL");
    }

    close_pipe(ok_pipe);
    close_pipe(bad_pipe);
    close(epfd);

    TEST_DONE();
}
