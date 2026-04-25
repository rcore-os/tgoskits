/*
 * test-ipv6-socket: 验证 AF_INET6 socket 完整 syscall 路径
 *
 * 测试覆盖 wget 发起 HTTP 连接所需的全部 syscall：
 *   socket(AF_INET6, SOCK_STREAM, 0)
 *   socket(AF_INET6, SOCK_DGRAM,  0)
 *   setsockopt / getsockopt (IPV6_V6ONLY, SO_REUSEADDR)
 *   bind(sockaddr_in6) / listen / accept / connect
 *   send / recv
 *   getsockname / getpeername（验证返回 AF_INET6 地址）
 *   shutdown
 *
 * 全部使用本地回环地址 ::1，无需外部网络。
 *
 * 通过条件：DONE: N pass, 0 fail
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>
#include <string.h>
#include <errno.h>

/* 每个子测试使用独立端口，避免 TIME_WAIT 干扰 */
#define BASE_PORT 28080
#define MSG       "hello ipv6"
#define MSGLEN    (sizeof(MSG))  /* 含末尾 '\0' */

/* ── 1. socket() 创建与关闭 ─────────────────────────────────────── */
static void test_socket_create(void)
{
    int fd;

    fd = socket(AF_INET6, SOCK_STREAM, 0);
    CHECK(fd >= 0, "socket(AF_INET6, SOCK_STREAM, 0)");
    if (fd >= 0) close(fd);

    fd = socket(AF_INET6, SOCK_STREAM, IPPROTO_TCP);
    CHECK(fd >= 0, "socket(AF_INET6, SOCK_STREAM, IPPROTO_TCP)");
    if (fd >= 0) close(fd);

    fd = socket(AF_INET6, SOCK_DGRAM, 0);
    CHECK(fd >= 0, "socket(AF_INET6, SOCK_DGRAM, 0)");
    if (fd >= 0) close(fd);

    fd = socket(AF_INET6, SOCK_DGRAM, IPPROTO_UDP);
    CHECK(fd >= 0, "socket(AF_INET6, SOCK_DGRAM, IPPROTO_UDP)");
    if (fd >= 0) close(fd);
}

/* ── 2. setsockopt / getsockopt (IPV6_V6ONLY) ───────────────────── */
static void test_sockopt_ipv6only(void)
{
    int fd = socket(AF_INET6, SOCK_STREAM, 0);
    if (fd < 0) { __fail++; return; }

    /* setsockopt IPV6_V6ONLY = 1 */
    int val = 1;
    CHECK_RET(setsockopt(fd, IPPROTO_IPV6, IPV6_V6ONLY, &val, sizeof(val)),
              0, "setsockopt(IPPROTO_IPV6, IPV6_V6ONLY, 1)");

    /* getsockopt should reflect the set value */
    int got = 0;
    socklen_t len = sizeof(got);
    CHECK_RET(getsockopt(fd, IPPROTO_IPV6, IPV6_V6ONLY, &got, &len),
              0, "getsockopt(IPPROTO_IPV6, IPV6_V6ONLY)");
    CHECK(got == 1, "IPV6_V6ONLY reads back 1");

    /* setsockopt IPV6_V6ONLY = 0 (dual-stack mode) */
    val = 0;
    CHECK_RET(setsockopt(fd, IPPROTO_IPV6, IPV6_V6ONLY, &val, sizeof(val)),
              0, "setsockopt(IPPROTO_IPV6, IPV6_V6ONLY, 0)");

    close(fd);
}

/* ── 3. SO_REUSEADDR on AF_INET6 socket ─────────────────────────── */
static void test_sockopt_reuseaddr(void)
{
    int fd = socket(AF_INET6, SOCK_STREAM, 0);
    if (fd < 0) { __fail++; return; }

    int val = 1;
    CHECK_RET(setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &val, sizeof(val)),
              0, "setsockopt(SOL_SOCKET, SO_REUSEADDR, 1)");

    int got = 0;
    socklen_t len = sizeof(got);
    CHECK_RET(getsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &got, &len),
              0, "getsockopt(SOL_SOCKET, SO_REUSEADDR)");
    CHECK(got != 0, "SO_REUSEADDR reads back non-zero");

    close(fd);
}

/* ── 4. bind(sockaddr_in6) 到 ::1 ───────────────────────────────── */
static void test_bind_loopback(void)
{
    int fd = socket(AF_INET6, SOCK_STREAM, 0);
    if (fd < 0) { __fail++; return; }

    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));

    struct sockaddr_in6 addr = {0};
    addr.sin6_family = AF_INET6;
    addr.sin6_port   = htons(BASE_PORT);
    addr.sin6_addr   = in6addr_loopback; /* ::1 */

    CHECK_RET(bind(fd, (struct sockaddr *)&addr, sizeof(addr)),
              0, "bind(AF_INET6, ::1, port)");

    close(fd);
}

/* ── 5. TCP 回环：listen / accept / connect / send / recv ────────── */
static void test_tcp_loopback(void)
{
    int server = socket(AF_INET6, SOCK_STREAM, 0);
    if (server < 0) { __fail++; return; }

    int one = 1;
    setsockopt(server, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));

    struct sockaddr_in6 addr = {0};
    addr.sin6_family = AF_INET6;
    addr.sin6_port   = htons(BASE_PORT + 1);
    addr.sin6_addr   = in6addr_loopback;

    if (bind(server, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
        printf("  FAIL | %s:%d | bind failed: errno=%d\n",
               __FILE__, __LINE__, errno);
        __fail++;
        close(server);
        return;
    }
    CHECK_RET(listen(server, 5), 0, "listen(AF_INET6 server)");

    pid_t pid = fork();
    if (pid == 0) {
        /* ── child: client ── */
        close(server);
        int c = socket(AF_INET6, SOCK_STREAM, 0);
        if (c < 0) _exit(1);
        if (connect(c, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
            close(c);
            _exit(1);
        }
        ssize_t n = send(c, MSG, MSGLEN, 0);
        close(c);
        _exit(n == (ssize_t)MSGLEN ? 0 : 1);
    }

    /* ── parent: server ── */
    struct sockaddr_in6 peer = {0};
    socklen_t plen = sizeof(peer);
    int conn = accept(server, (struct sockaddr *)&peer, &plen);
    CHECK(conn >= 0, "accept() returns valid fd");

    if (conn >= 0) {
        /* peer 地址族必须是 AF_INET6 */
        CHECK(peer.sin6_family == AF_INET6,
              "accepted peer.sin6_family == AF_INET6");

        /* recv 数据 */
        char buf[64] = {0};
        ssize_t n = recv(conn, buf, sizeof(buf), 0);
        CHECK(n == (ssize_t)MSGLEN, "recv: correct length");
        CHECK(memcmp(buf, MSG, MSGLEN) == 0, "recv: correct data");

        /* getsockname：获取本端地址 */
        struct sockaddr_in6 local = {0};
        socklen_t llen = sizeof(local);
        CHECK_RET(getsockname(conn, (struct sockaddr *)&local, &llen),
                  0, "getsockname on accepted conn");
        CHECK(local.sin6_family == AF_INET6,
              "getsockname: local.sin6_family == AF_INET6");
        CHECK(ntohs(local.sin6_port) == (BASE_PORT + 1),
              "getsockname: local port matches bind port");

        /* getpeername：获取对端地址 */
        struct sockaddr_in6 remote = {0};
        socklen_t rlen = sizeof(remote);
        CHECK_RET(getpeername(conn, (struct sockaddr *)&remote, &rlen),
                  0, "getpeername on accepted conn");
        CHECK(remote.sin6_family == AF_INET6,
              "getpeername: remote.sin6_family == AF_INET6");

        close(conn);
    }

    close(server);

    int status = 0;
    pid_t waited;
    do { waited = waitpid(pid, &status, 0); } while (waited == -1 && errno == EINTR);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "client child: connect + send succeeded");
}

/* ── 6. getsockname 在 bind 之后（未 accept 前）─────────────────── */
static void test_getsockname_bound(void)
{
    int fd = socket(AF_INET6, SOCK_STREAM, 0);
    if (fd < 0) { __fail++; return; }

    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));

    struct sockaddr_in6 addr = {0};
    addr.sin6_family = AF_INET6;
    addr.sin6_port   = htons(BASE_PORT + 2);
    addr.sin6_addr   = in6addr_loopback;
    bind(fd, (struct sockaddr *)&addr, sizeof(addr));

    struct sockaddr_in6 out = {0};
    socklen_t olen = sizeof(out);
    CHECK_RET(getsockname(fd, (struct sockaddr *)&out, &olen),
              0, "getsockname after bind");
    CHECK(out.sin6_family == AF_INET6,
          "getsockname: family == AF_INET6");
    CHECK(ntohs(out.sin6_port) == (BASE_PORT + 2),
          "getsockname: port matches");

    close(fd);
}

/* ── 7. shutdown on AF_INET6 listening socket ───────────────────── */
static void test_shutdown(void)
{
    int fd = socket(AF_INET6, SOCK_STREAM, 0);
    if (fd < 0) { __fail++; return; }

    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));

    struct sockaddr_in6 addr = {0};
    addr.sin6_family = AF_INET6;
    addr.sin6_port   = htons(BASE_PORT + 3);
    addr.sin6_addr   = in6addr_loopback;
    bind(fd, (struct sockaddr *)&addr, sizeof(addr));
    listen(fd, 1);

    CHECK_RET(shutdown(fd, SHUT_RD),   0, "shutdown(SHUT_RD)");
    CHECK_RET(shutdown(fd, SHUT_WR),   0, "shutdown(SHUT_WR)");

    close(fd);
}

/* ── 8. UDP over IPv6 loopback ──────────────────────────────────── */
static void test_udp_loopback(void)
{
    int server = socket(AF_INET6, SOCK_DGRAM, 0);
    if (server < 0) { __fail++; return; }

    struct sockaddr_in6 addr = {0};
    addr.sin6_family = AF_INET6;
    addr.sin6_port   = htons(BASE_PORT + 4);
    addr.sin6_addr   = in6addr_loopback;

    CHECK_RET(bind(server, (struct sockaddr *)&addr, sizeof(addr)),
              0, "UDP bind(AF_INET6, ::1)");

    int client = socket(AF_INET6, SOCK_DGRAM, 0);
    CHECK(client >= 0, "UDP client socket(AF_INET6, SOCK_DGRAM)");
    if (client < 0) { close(server); return; }

    /* sendto */
    ssize_t n = sendto(client, MSG, MSGLEN, 0,
                       (struct sockaddr *)&addr, sizeof(addr));
    CHECK(n == (ssize_t)MSGLEN, "UDP sendto ::1");

    /* recvfrom */
    char buf[64] = {0};
    struct sockaddr_in6 from = {0};
    socklen_t fromlen = sizeof(from);
    n = recvfrom(server, buf, sizeof(buf), 0,
                 (struct sockaddr *)&from, &fromlen);
    CHECK(n == (ssize_t)MSGLEN, "UDP recvfrom: correct length");
    CHECK(memcmp(buf, MSG, MSGLEN) == 0, "UDP recvfrom: correct data");
    CHECK(from.sin6_family == AF_INET6,
          "UDP recvfrom: sender.sin6_family == AF_INET6");

    close(client);
    close(server);
}

/* ── 9. connect + getsockname（auto-bind 后本地端口非零）────────── */
static void test_connect_autobind(void)
{
    /* 先建一个 TCP 服务端监听 ::1 */
    int server = socket(AF_INET6, SOCK_STREAM, 0);
    if (server < 0) { __fail++; return; }

    int one = 1;
    setsockopt(server, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));

    struct sockaddr_in6 saddr = {0};
    saddr.sin6_family = AF_INET6;
    saddr.sin6_port   = htons(BASE_PORT + 5);
    saddr.sin6_addr   = in6addr_loopback;

    if (bind(server, (struct sockaddr *)&saddr, sizeof(saddr)) != 0 ||
        listen(server, 5) != 0) {
        close(server);
        __fail++;
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        /* child: just accept and close */
        struct sockaddr_in6 peer;
        socklen_t plen = sizeof(peer);
        int conn = accept(server, (struct sockaddr *)&peer, &plen);
        if (conn >= 0) close(conn);
        close(server);
        _exit(0);
    }

    close(server);

    /* parent: connect, then getsockname to check auto-bound port */
    int c = socket(AF_INET6, SOCK_STREAM, 0);
    if (c < 0) { __fail++; goto wait; }

    CHECK_RET(connect(c, (struct sockaddr *)&saddr, sizeof(saddr)),
              0, "connect to ::1");

    struct sockaddr_in6 local = {0};
    socklen_t llen = sizeof(local);
    CHECK_RET(getsockname(c, (struct sockaddr *)&local, &llen),
              0, "getsockname after connect");
    CHECK(local.sin6_family == AF_INET6,
          "getsockname after connect: AF_INET6");
    CHECK(ntohs(local.sin6_port) != 0,
          "getsockname after connect: auto-bound port != 0");

    close(c);

wait:
    {
        int status = 0;
        pid_t waited;
        do { waited = waitpid(pid, &status, 0); } while (waited == -1 && errno == EINTR);
    }
}

int main(void)
{
    TEST_START("AF_INET6 socket syscalls (wget path)");

    test_socket_create();
    test_sockopt_ipv6only();
    test_sockopt_reuseaddr();
    test_bind_loopback();
    test_tcp_loopback();
    test_getsockname_bound();
    test_shutdown();
    test_udp_loopback();
    test_connect_autobind();

    TEST_DONE();
}
