/*
 * !test-nonblock-connect — 非阻塞 connect + epoll EPOLLOUT + SO_ERROR 穷尽测试
 * (浏览器/nginx 事件循环并发建连命脉)
 *
 * ground truth: man 2 connect "EINPROGRESS" + man 7 epoll + Linux v7.2
 * net/ipv4/af_inet.c inet_stream_connect。浏览器(Chromium/Firefox)与 nginx 用
 * 非阻塞 connect + epoll 一次并发发起大量连接, 由 EPOLLOUT 得知连接完成。
 *
 * =====================================================================
 * 语义 (man 2 connect)
 * =====================================================================
 *   非阻塞 socket connect(): 若立即完成返回 0; 否则 -1/EINPROGRESS。
 *   之后 epoll/poll EPOLLOUT 就绪表示握手完成; getsockopt(SO_ERROR) 读结果
 *   (0=成功, 否则连接错误如 ECONNREFUSED)。
 *
 * =====================================================================
 * Linux/StarryOS 对齐
 * =====================================================================
 *   inet_stream_connect: 非阻塞下 tcp_connect 发 SYN, 返回 EINPROGRESS;
 *   握手完成后 socket 变可写, epoll 报 EPOLLOUT; SO_ERROR 记录结果。
 * =====================================================================
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include "test_framework.h"

#include <stddef.h>
#include <stdint.h>
#include <sys/socket.h>
#include <sys/epoll.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <fcntl.h>
#include <signal.h>
#include <unistd.h>
#include <string.h>

#define NCONN 8

static void alarm_handler(int s)
{
    (void)s;
    const char *m = "\n  FAIL | TIMEOUT | 测试挂死\n==== test-nonblock-connect 汇总: FAIL ====\n";
    ssize_t r = write(2, m, strlen(m));
    (void)r;
    _exit(1);
}

static int set_nonblock(int fd)
{
    int fl = fcntl(fd, F_GETFL);
    return fl == -1 ? -1 : fcntl(fd, F_SETFL, fl | O_NONBLOCK);
}

/* 建一个监听 127.0.0.1 的 socket, 返回 fd; *port_out=分配端口。 */
static int make_listener(uint16_t *port_out)
{
    int ls = socket(AF_INET, SOCK_STREAM, 0);
    if (ls < 0) return -1;
    int one = 1;
    setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
    struct sockaddr_in a;
    memset(&a, 0, sizeof(a));
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = 0;
    if (bind(ls, (struct sockaddr *)&a, sizeof(a)) != 0) { close(ls); return -1; }
    if (listen(ls, 16) != 0) { close(ls); return -1; }
    socklen_t al = sizeof(a);
    if (getsockname(ls, (struct sockaddr *)&a, &al) != 0) { close(ls); return -1; }
    *port_out = a.sin_port;
    return ls;
}

/* 分配一个已 bind 但随即 close 的 loopback 端口, 使其无 listener -> connect 应被拒绝。
 * 返回 network-order port; 失败返回 0。 */
static uint16_t alloc_dead_port(void)
{
    int s = socket(AF_INET, SOCK_STREAM, 0);
    if (s < 0) return 0;
    struct sockaddr_in a;
    memset(&a, 0, sizeof(a));
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = 0;
    if (bind(s, (struct sockaddr *)&a, sizeof(a)) != 0) { close(s); return 0; }
    socklen_t al = sizeof(a);
    if (getsockname(s, (struct sockaddr *)&a, &al) != 0) { close(s); return 0; }
    close(s);
    return a.sin_port;
}

/* ===== A. 单个非阻塞 connect: EINPROGRESS -> EPOLLOUT -> SO_ERROR==0 ===== */
static int test_nonblock_connect_single(void)
{
    TEST_START("A. 非阻塞 connect EINPROGRESS -> EPOLLOUT -> SO_ERROR==0");
    uint16_t port = 0;
    int ls = make_listener(&port);
    CHECK(ls >= 0, "建 loopback listener");
    if (ls < 0) { TEST_DONE(); }

    int cs = socket(AF_INET, SOCK_STREAM, 0);
    set_nonblock(cs);
    struct sockaddr_in a;
    memset(&a, 0, sizeof(a));
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = port;

    errno = 0;
    int r = connect(cs, (struct sockaddr *)&a, sizeof(a));
    /* loopback 可能立即完成(返回0)或 EINPROGRESS, 二者皆合法 */
    CHECK(r == 0 || (r == -1 && errno == EINPROGRESS), "非阻塞 connect -> 0 或 EINPROGRESS");

    /* epoll 等 EPOLLOUT(连接完成) */
    int ep = epoll_create1(0);
    struct epoll_event e = { .events = EPOLLOUT, .data.fd = cs };
    epoll_ctl(ep, EPOLL_CTL_ADD, cs, &e);
    struct epoll_event out[4];
    int n = epoll_wait(ep, out, 4, 3000);
    CHECK(n == 1 && (out[0].events & EPOLLOUT), "epoll 报 EPOLLOUT(连接完成可写)");

    /* SO_ERROR == 0 确认连接成功 */
    int soerr = -1;
    socklen_t sl = sizeof(soerr);
    CHECK(getsockopt(cs, SOL_SOCKET, SO_ERROR, &soerr, &sl) == 0 && soerr == 0,
          "getsockopt SO_ERROR == 0(连接成功)");

    /* listener accept 到该连接 */
    int as = accept(ls, NULL, NULL);
    CHECK(as >= 0, "listener accept 到连接");

    if (as >= 0) close(as);
    close(ep);
    close(cs);
    close(ls);
    TEST_DONE();
}

/* ===== B. 高并发: 一次发起 NCONN 个非阻塞 connect, epoll 全部收敛 ===== */
static int test_nonblock_connect_concurrent(void)
{
    TEST_START("B. 高并发非阻塞 connect(事件循环批量建连)");
    uint16_t port = 0;
    int ls = make_listener(&port);
    if (ls < 0) { CHECK(0, "listener"); TEST_DONE(); }

    struct sockaddr_in a;
    memset(&a, 0, sizeof(a));
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = port;

    int ep = epoll_create1(0);
    int cs[NCONN];
    int launched = 0;
    for (int i = 0; i < NCONN; i++) {
        cs[i] = socket(AF_INET, SOCK_STREAM, 0);
        set_nonblock(cs[i]);
        int r = connect(cs[i], (struct sockaddr *)&a, sizeof(a));
        if (r == 0 || (r == -1 && errno == EINPROGRESS)) {
            struct epoll_event e = { .events = EPOLLOUT, .data.fd = cs[i] };
            epoll_ctl(ep, EPOLL_CTL_ADD, cs[i], &e);
            launched++;
        }
    }
    CHECK(launched == NCONN, "批量发起 NCONN 个非阻塞 connect");

    /* accept 所有 + 等所有 EPOLLOUT 收敛 */
    int connected = 0;
    int accepted = 0;
    int spins = 0;
    while (connected < launched && spins++ < 200) {
        int as = accept(ls, NULL, NULL);
        if (as >= 0) { accepted++; close(as); }
        struct epoll_event out[NCONN];
        int n = epoll_wait(ep, out, NCONN, 100);
        for (int i = 0; i < n; i++) {
            if (out[i].events & EPOLLOUT) {
                int soerr = -1;
                socklen_t sl = sizeof(soerr);
                getsockopt(out[i].data.fd, SOL_SOCKET, SO_ERROR, &soerr, &sl);
                if (soerr == 0) {
                    epoll_ctl(ep, EPOLL_CTL_DEL, out[i].data.fd, NULL);
                    connected++;
                }
            }
        }
    }
    CHECK(connected == launched, "所有并发连接经 EPOLLOUT 收敛完成");
    CHECK(accepted >= 1, "listener accept 到并发连接");

    for (int i = 0; i < NCONN; i++) close(cs[i]);
    close(ep);
    close(ls);
    TEST_DONE();
}

/* ===== C. P0 非阻塞 connect 失败路径 SO_ERROR==ECONNREFUSED ===== */
static int test_nonblock_connect_refused(void)
{
    TEST_START("C. 非阻塞 connect 失败 -> EPOLLOUT|EPOLLERR + SO_ERROR==ECONNREFUSED");
    uint16_t dead = alloc_dead_port();
    CHECK(dead != 0, "分配无 listener 的 loopback 端口");
    if (dead == 0) { TEST_DONE(); }

    int cs = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(cs >= 0, "建非阻塞客户端 socket");
    set_nonblock(cs);
    struct sockaddr_in a;
    memset(&a, 0, sizeof(a));
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = dead;

    errno = 0;
    int r = connect(cs, (struct sockaddr *)&a, sizeof(a));
    /* loopback 无 listener: 可能立即 -1/ECONNREFUSED, 也可能 -1/EINPROGRESS 后经 epoll 报错 */
    CHECK(r == -1 && (errno == EINPROGRESS || errno == ECONNREFUSED),
          "非阻塞 connect 无 listener -> EINPROGRESS 或 ECONNREFUSED");
    int immediate_refused = (r == -1 && errno == ECONNREFUSED);

    if (!immediate_refused) {
        int ep = epoll_create1(0);
        struct epoll_event e = { .events = EPOLLOUT, .data.fd = cs };
        epoll_ctl(ep, EPOLL_CTL_ADD, cs, &e);
        struct epoll_event out[4];
        int n = epoll_wait(ep, out, 4, 3000);
        /* man: select/poll 报可写以指示完成(含失败); epoll 报 EPOLLOUT, 通常伴 EPOLLERR/EPOLLHUP */
        CHECK(n == 1 && (out[0].events & (EPOLLOUT | EPOLLERR | EPOLLHUP)),
              "epoll 就绪报告连接失败(EPOLLOUT/EPOLLERR/EPOLLHUP)");
        close(ep);
    }

    /* SO_ERROR 应记录 ECONNREFUSED (man NOTES) */
    int soerr = -1;
    socklen_t sl = sizeof(soerr);
    int gr = getsockopt(cs, SOL_SOCKET, SO_ERROR, &soerr, &sl);
    if (immediate_refused) {
        /* 立即失败路径: 错误已由 connect 返回消费, SO_ERROR 通常为 0; 仅断言 getsockopt 成功 */
        CHECK(gr == 0, "getsockopt SO_ERROR 调用成功");
    } else {
        CHECK(gr == 0 && soerr == ECONNREFUSED,
              "getsockopt SO_ERROR == ECONNREFUSED(连接被拒的真实错误码)");
    }

    close(cs);
    TEST_DONE();
}

/* ===== D. 阻塞 connect 成功基准 + EISCONN 重连 ===== */
static int test_blocking_connect_and_eisconn(void)
{
    TEST_START("D. 阻塞 connect 成功返回 0 + 已连接再 connect -> EISCONN");
    uint16_t port = 0;
    int ls = make_listener(&port);
    CHECK(ls >= 0, "建 loopback listener");
    if (ls < 0) { TEST_DONE(); }

    int cs = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(cs >= 0, "建阻塞客户端 socket");
    struct sockaddr_in a;
    memset(&a, 0, sizeof(a));
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = port;

    /* man RETURN VALUE: 连接成功 -> 返回 0 */
    CHECK_RET(connect(cs, (struct sockaddr *)&a, sizeof(a)), 0,
              "阻塞 connect 成功返回 0");

    int as = accept(ls, NULL, NULL);
    CHECK(as >= 0, "listener accept 阻塞连接");

    /* man ERRORS: 已连接 socket 再次 connect -> EISCONN */
    CHECK_ERR(connect(cs, (struct sockaddr *)&a, sizeof(a)), EISCONN,
              "对已连接 socket 再 connect -> EISCONN");

    if (as >= 0) close(as);
    close(cs);
    close(ls);
    TEST_DONE();
}

/* ===== E. 阻塞 connect ECONNREFUSED 无 listener ===== */
static int test_blocking_connect_refused(void)
{
    TEST_START("E. 阻塞 connect 无 listener 端口 -> ECONNREFUSED");
    uint16_t dead = alloc_dead_port();
    CHECK(dead != 0, "分配无 listener 的 loopback 端口");
    if (dead == 0) { TEST_DONE(); }

    int cs = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(cs >= 0, "建阻塞客户端 socket");
    struct sockaddr_in a;
    memset(&a, 0, sizeof(a));
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = dead;

    /* man ERRORS: stream socket connect 无监听者 -> ECONNREFUSED */
    CHECK_ERR(connect(cs, (struct sockaddr *)&a, sizeof(a)), ECONNREFUSED,
              "阻塞 connect 无 listener -> ECONNREFUSED");

    close(cs);
    TEST_DONE();
}

/* ===== F. connect 基础 errno: EBADF / ENOTSOCK / EAFNOSUPPORT ===== */
static int test_connect_errno_matrix(void)
{
    TEST_START("F. connect 基础 errno 矩阵: EBADF / ENOTSOCK / EAFNOSUPPORT");
    struct sockaddr_in a;
    memset(&a, 0, sizeof(a));
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = htons(9);

    /* man ERRORS EBADF: sockfd 非有效打开的 fd */
    CHECK_ERR(connect(-1, (struct sockaddr *)&a, sizeof(a)), EBADF,
              "connect(-1, ...) -> EBADF");

    /* 已 close 的 fd 也应 EBADF */
    int tmp = socket(AF_INET, SOCK_STREAM, 0);
    if (tmp >= 0) {
        close(tmp);
        CHECK_ERR(connect(tmp, (struct sockaddr *)&a, sizeof(a)), EBADF,
                  "connect(已 close 的 fd) -> EBADF");
    }

    /* man ERRORS ENOTSOCK: fd 不指向 socket (普通文件 fd) */
    int ffd = open("/dev/null", O_RDONLY);
    CHECK(ffd >= 0, "打开 /dev/null 取普通文件 fd");
    if (ffd >= 0) {
        CHECK_ERR(connect(ffd, (struct sockaddr *)&a, sizeof(a)), ENOTSOCK,
                  "connect(普通文件 fd) -> ENOTSOCK");
        close(ffd);
    }

    /* man ERRORS EAFNOSUPPORT: sa_family 地址族错误 */
    int cs = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(cs >= 0, "建 AF_INET socket");
    if (cs >= 0) {
        struct sockaddr_in bad = a;
        bad.sin_family = AF_UNIX; /* AF_INET socket 上给 AF_UNIX 地址族 */
        CHECK_ERR(connect(cs, (struct sockaddr *)&bad, sizeof(bad)), EAFNOSUPPORT,
                  "connect(sa_family=AF_UNIX 于 AF_INET socket) -> EAFNOSUPPORT");
        close(cs);
    }

    TEST_DONE();
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    signal(SIGPIPE, SIG_IGN);
    signal(SIGALRM, alarm_handler);
    alarm(60);
    int fail = 0;
    fail |= test_nonblock_connect_single();
    fail |= test_nonblock_connect_concurrent();
    fail |= test_nonblock_connect_refused();
    fail |= test_blocking_connect_and_eisconn();
    fail |= test_blocking_connect_refused();
    fail |= test_connect_errno_matrix();
    printf("\n==== test-nonblock-connect 汇总: %s ====\n", fail ? "FAIL" : "PASS");
    return fail;
}
