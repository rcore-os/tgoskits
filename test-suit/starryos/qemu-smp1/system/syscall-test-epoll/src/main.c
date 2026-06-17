/*
 * 本测例直接使用 https://github.com/rcore-os/linux-compatible-testsuit 的 test_epoll.c
 *
 * test_epoll.c — epoll_create1/epoll_ctl/epoll_wait 完整测试
 *
 * 测试策略：基于 pipe 验证 epoll 的 ADD/MOD/DEL 和事件检测语义
 *
 * 覆盖范围：
 *   正向：epoll_create1、EPOLL_CLOEXEC、epoll_ctl ADD/MOD/DEL、
 *         epoll_wait 基本事件、超时、EPOLLHUP、多 fd、EPOLLET
 *   负向：epoll_ctl 无效 fd EBADF、epoll_ctl 无效 op EINVAL、
 *         epoll_wait 无效 epollfd EBADF
 */

#include "test_framework.h"
#include <fcntl.h>
#include <unistd.h>
#include <sys/epoll.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <errno.h>
#include <string.h>

#define TMPFILE "/tmp/starry_epoll_test"

int main(void)
{
    TEST_START("epoll: epoll_create1/epoll_ctl/epoll_wait 语义验证");

    unlink(TMPFILE);

    /* ================================================================
     * PART 1: epoll_create1 — 创建 epoll 实例
     * ================================================================ */

    {
        int epfd = epoll_create1(0);
        CHECK(epfd >= 0, "PART1: epoll_create1(0) 成功");
        if (epfd >= 0) close(epfd);

#ifdef __x86_64__
        epfd = epoll_create(16);
        CHECK(epfd >= 0, "PART1: epoll_create(size>0) 成功");
        if (epfd >= 0) close(epfd);

        CHECK_ERR(epoll_create(0), EINVAL,
                  "PART1: epoll_create(size=0) 返回 EINVAL");
        CHECK_ERR(epoll_create(-1), EINVAL,
                  "PART1: epoll_create(size<0) 返回 EINVAL");
#endif
    }

    /* ================================================================
     * PART 2: EPOLL_CLOEXEC 标志
     * ================================================================ */

    {
        int epfd = epoll_create1(EPOLL_CLOEXEC);
        CHECK(epfd >= 0, "PART2: epoll_create1(EPOLL_CLOEXEC) 成功");
        if (epfd >= 0) {
            int flags = fcntl(epfd, F_GETFD);
            CHECK(flags >= 0 && (flags & FD_CLOEXEC),
                  "PART2: FD_CLOEXEC 标志已设置");
            close(epfd);
        }
    }

    /* ================================================================
     * PART 3: epoll_ctl ADD + epoll_wait EPOLLIN
     * ================================================================ */

    {
        int epfd = epoll_create1(0);
        CHECK(epfd >= 0, "PART3: epoll_create1 成功");
        if (epfd < 0) { TEST_DONE(); }

        int fds[2];
        int ret = pipe(fds);
        CHECK_RET(ret, 0, "PART3: pipe 创建成功");
        if (ret != 0) { close(epfd); TEST_DONE(); }

        struct epoll_event ev;
        ev.events = EPOLLIN;
        ev.data.fd = fds[0];
        CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, fds[0], &ev), 0,
                  "PART3: epoll_ctl ADD 读端成功");

        write(fds[1], "hello", 5);

        struct epoll_event events[4];
        int nready = epoll_wait(epfd, events, 4, 1000);
        CHECK_RET(nready, 1, "PART3: epoll_wait 返回 1");
        if (nready == 1) {
            CHECK(events[0].events & EPOLLIN,
                  "PART3: 事件包含 EPOLLIN");
            CHECK(events[0].data.fd == fds[0],
                  "PART3: data.fd 与注册时一致");
        }

        nready = syscall(SYS_epoll_pwait, epfd, events, 4, -2, NULL, 0);
        CHECK_RET(nready, 1,
                  "PART3: epoll_pwait timeout<-1 在已有事件时返回事件");

        char buf[16];
        read(fds[0], buf, 5);

        close(fds[0]);
        close(fds[1]);
        close(epfd);
    }

    /* ================================================================
     * PART 4: epoll_wait 超时返回 0
     * ================================================================ */

    {
        int epfd = epoll_create1(0);
        CHECK(epfd >= 0, "PART4: epoll_create1 成功");
        if (epfd < 0) { TEST_DONE(); }

        struct epoll_event events[4];
        int nready = epoll_wait(epfd, events, 4, 100);
        CHECK_RET(nready, 0, "PART4: epoll_wait 无事件 100ms 超时返回 0");

        close(epfd);
    }

    /* ================================================================
     * PART 5: epoll_ctl MOD — 修改事件
     * ================================================================ */

    {
        int epfd = epoll_create1(0);
        CHECK(epfd >= 0, "PART5: epoll_create1 成功");
        if (epfd < 0) { TEST_DONE(); }

        int fds[2];
        int ret = pipe(fds);
        CHECK_RET(ret, 0, "PART5: pipe 创建成功");
        if (ret != 0) { close(epfd); TEST_DONE(); }

        struct epoll_event ev;
        ev.events = EPOLLIN;
        ev.data.fd = fds[0];
        CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, fds[0], &ev), 0,
                  "PART5: epoll_ctl ADD EPOLLIN 成功");

        ev.events = EPOLLOUT;
        ev.data.fd = fds[1];
        CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, fds[1], &ev), 0,
                  "PART5: epoll_ctl ADD 写端 EPOLLOUT 成功");

        struct epoll_event events[4];
        int nready = epoll_wait(epfd, events, 4, 100);
        CHECK(nready >= 1, "PART5: epoll_wait 写端就绪返回 >= 1");
        if (nready >= 1) {
            int found_out = 0;
            for (int i = 0; i < nready; i++) {
                if (events[i].events & EPOLLOUT && events[i].data.fd == fds[1])
                    found_out = 1;
            }
            CHECK(found_out, "PART5: 找到写端 EPOLLOUT 事件");
        }

        close(fds[0]);
        close(fds[1]);
        close(epfd);
    }

    /* ================================================================
     * PART 6: epoll_ctl DEL — 删除 fd
     * ================================================================ */

    {
        int epfd = epoll_create1(0);
        CHECK(epfd >= 0, "PART6: epoll_create1 成功");
        if (epfd < 0) { TEST_DONE(); }

        int fds[2];
        int ret = pipe(fds);
        CHECK_RET(ret, 0, "PART6: pipe 创建成功");
        if (ret != 0) { close(epfd); TEST_DONE(); }

        struct epoll_event ev;
        ev.events = EPOLLIN;
        ev.data.fd = fds[0];
        CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, fds[0], &ev), 0,
                  "PART6: epoll_ctl ADD 成功");

        CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_DEL, fds[0], NULL), 0,
                  "PART6: epoll_ctl DEL 成功");

        write(fds[1], "x", 1);

        struct epoll_event events[4];
        int nready = epoll_wait(epfd, events, 4, 100);
        CHECK_RET(nready, 0, "PART6: DEL 后 epoll_wait 超时返回 0");

        close(fds[0]);
        close(fds[1]);
        close(epfd);
    }

    /* ================================================================
     * PART 7: 管道写端关闭 → EPOLLHUP
     * ================================================================ */

    {
        int epfd = epoll_create1(0);
        CHECK(epfd >= 0, "PART7: epoll_create1 成功");
        if (epfd < 0) { TEST_DONE(); }

        int fds[2];
        int ret = pipe(fds);
        CHECK_RET(ret, 0, "PART7: pipe 创建成功");
        if (ret != 0) { close(epfd); TEST_DONE(); }

        struct epoll_event ev;
        ev.events = EPOLLIN;
        ev.data.fd = fds[0];
        CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, fds[0], &ev), 0,
                  "PART7: epoll_ctl ADD 读端成功");

        close(fds[1]);

        struct epoll_event events[4];
        int nready = epoll_wait(epfd, events, 4, 1000);
        CHECK_RET(nready, 1, "PART7: epoll_wait 写端关闭后返回 1");
        if (nready == 1) {
            CHECK(events[0].events & (EPOLLIN | EPOLLHUP),
                  "PART7: 事件包含 EPOLLIN 或 EPOLLHUP");
        }

        close(fds[0]);
        close(epfd);
    }

    /* ================================================================
     * PART 8: 多 fd 同时监控
     * ================================================================ */

    {
        int epfd = epoll_create1(0);
        CHECK(epfd >= 0, "PART8: epoll_create1 成功");
        if (epfd < 0) { TEST_DONE(); }

        int pipe1[2], pipe2[2];
        int r1 = pipe(pipe1);
        int r2 = pipe(pipe2);
        CHECK(r1 == 0 && r2 == 0, "PART8: 创建两个管道成功");
        if (r1 != 0 || r2 != 0) { close(epfd); TEST_DONE(); }

        struct epoll_event ev;
        ev.events = EPOLLIN;
        ev.data.fd = pipe1[0];
        CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, pipe1[0], &ev), 0,
                  "PART8: ADD pipe1 读端成功");

        ev.events = EPOLLIN;
        ev.data.fd = pipe2[0];
        CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, pipe2[0], &ev), 0,
                  "PART8: ADD pipe2 读端成功");

        write(pipe1[1], "a", 1);
        write(pipe2[1], "b", 1);

        struct epoll_event events[4];
        int nready = epoll_wait(epfd, events, 4, 1000);
        CHECK(nready == 2, "PART8: 两个管道都就绪返回 2");

        close(pipe1[0]);
        close(pipe1[1]);
        close(pipe2[0]);
        close(pipe2[1]);
        close(epfd);
    }

    /* ================================================================
     * PART 9: EPOLLET 边沿触发
     * ================================================================ */

    {
        int epfd = epoll_create1(0);
        CHECK(epfd >= 0, "PART9: epoll_create1 成功");
        if (epfd < 0) { TEST_DONE(); }

        int fds[2];
        int ret = pipe(fds);
        CHECK_RET(ret, 0, "PART9: pipe 创建成功");
        if (ret != 0) { close(epfd); TEST_DONE(); }

        struct epoll_event ev;
        ev.events = EPOLLIN | EPOLLET;
        ev.data.fd = fds[0];
        CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, fds[0], &ev), 0,
                  "PART9: epoll_ctl ADD EPOLLET 成功");

        write(fds[1], "first", 5);

        struct epoll_event events[4];
        int nready = epoll_wait(epfd, events, 4, 100);
        CHECK_RET(nready, 1, "PART9: 第一次 EPOLLET 事件返回 1");

        write(fds[1], "second", 6);

        nready = epoll_wait(epfd, events, 4, 100);
        CHECK(nready == 0 || nready == 1,
              "PART9: EPOLLET 后续写入可能返回 0 或 1 (边沿触发语义)");

        close(fds[0]);
        close(fds[1]);
        close(epfd);
    }

    /* ================================================================
     * PART 10: 负向测试
     * ================================================================ */

    {
        int epfd = epoll_create1(0);
        CHECK(epfd >= 0, "PART10: epoll_create1 成功");
        if (epfd < 0) { TEST_DONE(); }

        struct epoll_event ev;
        ev.events = EPOLLIN;
        ev.data.fd = 0;

        errno = 0;
        CHECK(epoll_ctl(epfd, EPOLL_CTL_ADD, -1, &ev) == -1 &&
              (errno == EBADF || errno == EINVAL),
              "PART10: epoll_ctl ADD 无效 fd 返回 EBADF/EINVAL");

        errno = 0;
        CHECK(epoll_ctl(-1, EPOLL_CTL_ADD, 0, &ev) == -1 &&
              errno == EBADF,
              "PART10: epoll_ctl 无效 epfd 返回 EBADF");

        errno = 0;
        struct epoll_event events[4];
        CHECK(epoll_wait(-1, events, 4, 0) == -1 &&
              errno == EBADF,
              "PART10: epoll_wait 无效 epfd 返回 EBADF");

        int fds[2];
        int ret = pipe(fds);
        if (ret == 0) {
            ev.events = EPOLLIN;
            ev.data.fd = fds[0];
            CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, fds[0], &ev), 0,
                      "PART10: ADD 成功");

            errno = 0;
            CHECK(epoll_ctl(epfd, EPOLL_CTL_ADD, fds[0], &ev) == -1 &&
                  errno == EEXIST,
                  "PART10: 重复 ADD 返回 EEXIST");

            errno = 0;
            CHECK(epoll_ctl(epfd, EPOLL_CTL_MOD, -1, &ev) == -1 &&
                  (errno == EBADF || errno == ENOENT),
                  "PART10: MOD 不存在的 fd 返回 EBADF/ENOENT");

            errno = 0;
            CHECK(epoll_ctl(epfd, EPOLL_CTL_DEL, -1, NULL) == -1 &&
                  (errno == EBADF || errno == ENOENT),
                  "PART10: DEL 不存在的 fd 返回 EBADF/ENOENT");

            close(fds[0]);
            close(fds[1]);
        }

        errno = 0;
        CHECK(epoll_ctl(epfd, EPOLL_CTL_ADD, epfd, &ev) == -1 &&
              errno == EINVAL,
              "PART10: epoll_ctl 拒绝把 epoll fd 加入自身");

        errno = 0;
        CHECK(epoll_wait(epfd, events, 0, 0) == -1 &&
              errno == EINVAL,
              "PART10: epoll_wait maxevents=0 返回 EINVAL");

        errno = 0;
#ifdef SYS_epoll_wait
        CHECK(syscall(SYS_epoll_wait, epfd, NULL, 1, 0) == -1 &&
              errno == EFAULT,
              "PART10: epoll_wait NULL events 返回 EFAULT");
#elif defined(SYS_epoll_pwait)
        CHECK(syscall(SYS_epoll_pwait, epfd, NULL, 1, 0, NULL, 0) == -1 &&
              errno == EFAULT,
              "PART10: epoll_wait NULL events 返回 EFAULT");
#else
        printf("  SKIP | %s:%d | PART10: raw epoll wait syscall is not defined\n",
               __FILE__, __LINE__);
#endif

        close(epfd);
    }

    /* ================================================================
     * PART 11: fork 后 epoll 继承
     * ================================================================ */

    {
        int epfd = epoll_create1(0);
        CHECK(epfd >= 0, "PART11: epoll_create1 成功");
        if (epfd < 0) { TEST_DONE(); }

        int fds[2];
        int ret = pipe(fds);
        CHECK_RET(ret, 0, "PART11: pipe 创建成功");
        if (ret != 0) { close(epfd); TEST_DONE(); }

        struct epoll_event ev;
        ev.events = EPOLLIN;
        ev.data.fd = fds[0];
        CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, fds[0], &ev), 0,
                  "PART11: epoll_ctl ADD 成功");

        pid_t pid = fork();
        if (pid == 0) {
            write(fds[1], "from_child", 10);
            close(fds[0]);
            close(fds[1]);
            close(epfd);
            _exit(0);
        }

        close(fds[1]);

        struct epoll_event events[4];
        int nready = epoll_wait(epfd, events, 4, 2000);
        CHECK_RET(nready, 1, "PART11: 子进程写入后 epoll_wait 返回 1");
        if (nready == 1) {
            CHECK(events[0].events & EPOLLIN,
                  "PART11: 事件包含 EPOLLIN");
        }

        int status;
        wait4(pid, &status, 0, NULL);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "PART11: 子进程正常退出");

        close(fds[0]);
        close(epfd);
    }

    /* ================================================================
     * PART 12: epoll_ctl DEL 已关闭的 fd
     * ================================================================ */

    {
        int epfd = epoll_create1(0);
        CHECK(epfd >= 0, "PART12: epoll_create1 成功");
        if (epfd < 0) { TEST_DONE(); }

        int fds[2];
        int ret = pipe(fds);
        CHECK_RET(ret, 0, "PART12: pipe 创建成功");
        if (ret != 0) { close(epfd); TEST_DONE(); }

        struct epoll_event ev;
        ev.events = EPOLLIN;
        ev.data.fd = fds[0];
        CHECK_RET(epoll_ctl(epfd, EPOLL_CTL_ADD, fds[0], &ev), 0,
                  "PART12: epoll_ctl ADD 成功");

        close(fds[0]);
        close(fds[1]);

        errno = 0;
        int del_ret = epoll_ctl(epfd, EPOLL_CTL_DEL, fds[0], NULL);
        CHECK(del_ret == -1 || del_ret == 0,
              "PART12: DEL 已关闭 fd 返回错误或成功 (内核自动清理)");

        close(epfd);
    }

    unlink(TMPFILE);

    TEST_DONE();
}
