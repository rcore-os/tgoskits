/*
 * test-ipv6-dualstack: 验证 AF_INET6 双栈模式（IPV6_V6ONLY=0）
 *
 * 背景：wget 在系统同时有 A 和 AAAA 记录时，会先创建 AF_INET6 socket，
 * 并通过 IPV6_V6ONLY=0（双栈）让同一个 socket 也能连接 IPv4 目标。
 * 这要求内核对 AF_INET6 socket 做 IPv4-mapped 地址转换：
 *   - 客户端通过 127.0.0.1 连接服务端
 *   - 服务端（AF_INET6 socket，IPV6_V6ONLY=0）accept 到的对端地址
 *     应为 ::ffff:127.0.0.1（IN6_IS_ADDR_V4MAPPED 为真）
 *
 * 测试分两部分：
 *   A. 纯 IPv6-only 模式（IPV6_V6ONLY=1）：AF_INET 客户端连接被拒绝
 *   B. 双栈模式（IPV6_V6ONLY=0）：AF_INET 客户端可成功连接
 *
 * 全部使用本地回环，无需外部网络。
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

#define BASE_PORT 28090
#define MSG       "dual"
#define MSGLEN    (sizeof(MSG))

/* 等待子进程并检查退出状态 */
static void wait_child(pid_t pid, const char *label)
{
    int status = 0;
    pid_t r;
    do { r = waitpid(pid, &status, 0); } while (r == -1 && errno == EINTR);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, label);
}

/* ── A. IPV6_V6ONLY = 1：AF_INET 客户端必须被拒绝 ──────────────── */
static void test_v6only_rejects_v4(void)
{
    int server = socket(AF_INET6, SOCK_STREAM, 0);
    if (server < 0) { __fail++; return; }

    int one = 1;
    setsockopt(server, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));

    /* 设置 IPV6_V6ONLY = 1：只接受纯 IPv6 连接 */
    CHECK_RET(setsockopt(server, IPPROTO_IPV6, IPV6_V6ONLY, &one, sizeof(one)),
              0, "setsockopt(IPV6_V6ONLY=1)");

    struct sockaddr_in6 saddr = {0};
    saddr.sin6_family = AF_INET6;
    saddr.sin6_port   = htons(BASE_PORT);
    saddr.sin6_addr   = in6addr_any; /* :: */

    if (bind(server, (struct sockaddr *)&saddr, sizeof(saddr)) != 0 ||
        listen(server, 5) != 0) {
        close(server);
        __fail++;
        return;
    }

    /* 子进程：AF_INET 客户端尝试连接 127.0.0.1 */
    pid_t pid = fork();
    if (pid == 0) {
        close(server);
        int c = socket(AF_INET, SOCK_STREAM, 0);
        if (c < 0) _exit(2);
        struct sockaddr_in v4 = {0};
        v4.sin_family      = AF_INET;
        v4.sin_port        = htons(BASE_PORT);
        v4.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        /* 连接应失败（IPV6_V6ONLY=1 → 服务端不接受 IPv4-mapped）*/
        int r = connect(c, (struct sockaddr *)&v4, sizeof(v4));
        close(c);
        /* 退出码 0：connect 失败（预期）；退出码 1：connect 成功（非预期）*/
        _exit(r != 0 ? 0 : 1);
    }

    /* 设置非阻塞，短暂等待后关闭（不期望 accept 成功）*/
    struct timeval tv = { .tv_sec = 2, .tv_usec = 0 };
    setsockopt(server, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));

    struct sockaddr_in6 peer = {0};
    socklen_t plen = sizeof(peer);
    int conn = accept(server, (struct sockaddr *)&peer, &plen);
    CHECK(conn < 0, "IPV6_V6ONLY=1: AF_INET client should NOT be accepted");
    if (conn >= 0) close(conn);

    close(server);
    wait_child(pid, "IPV6_V6ONLY=1: AF_INET client connect should fail");
}

/* ── B. IPV6_V6ONLY = 0：AF_INET 客户端必须成功连接 ────────────── */
static void test_dualstack_accepts_v4(void)
{
    int server = socket(AF_INET6, SOCK_STREAM, 0);
    if (server < 0) { __fail++; return; }

    int one = 1;
    setsockopt(server, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));

    /* 设置 IPV6_V6ONLY = 0：双栈模式，接受 IPv4-mapped 连接 */
    int zero = 0;
    CHECK_RET(setsockopt(server, IPPROTO_IPV6, IPV6_V6ONLY, &zero, sizeof(zero)),
              0, "setsockopt(IPV6_V6ONLY=0)");

    struct sockaddr_in6 saddr = {0};
    saddr.sin6_family = AF_INET6;
    saddr.sin6_port   = htons(BASE_PORT + 1);
    saddr.sin6_addr   = in6addr_any; /* :: */

    if (bind(server, (struct sockaddr *)&saddr, sizeof(saddr)) != 0 ||
        listen(server, 5) != 0) {
        close(server);
        __fail++;
        return;
    }

    /* 子进程：AF_INET 客户端连接 127.0.0.1 */
    pid_t pid = fork();
    if (pid == 0) {
        close(server);
        int c = socket(AF_INET, SOCK_STREAM, 0);
        if (c < 0) _exit(1);
        struct sockaddr_in v4 = {0};
        v4.sin_family      = AF_INET;
        v4.sin_port        = htons(BASE_PORT + 1);
        v4.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        if (connect(c, (struct sockaddr *)&v4, sizeof(v4)) != 0) {
            close(c); _exit(1);
        }
        ssize_t n = send(c, MSG, MSGLEN, 0);
        close(c);
        _exit(n == (ssize_t)MSGLEN ? 0 : 1);
    }

    /* 服务端 accept */
    struct sockaddr_in6 peer = {0};
    socklen_t plen = sizeof(peer);
    int conn = accept(server, (struct sockaddr *)&peer, &plen);
    CHECK(conn >= 0, "IPV6_V6ONLY=0: accept() from AF_INET client succeeded");

    if (conn >= 0) {
        /* 对端地址必须是 AF_INET6 且为 IPv4-mapped（::ffff:127.0.0.1）*/
        CHECK(peer.sin6_family == AF_INET6,
              "peer.sin6_family == AF_INET6 (IPv4-mapped)");
        CHECK(IN6_IS_ADDR_V4MAPPED(&peer.sin6_addr),
              "peer address is IPv4-mapped (::ffff:127.0.0.1)");

        /* 验证数据收发正常 */
        char buf[32] = {0};
        ssize_t n = recv(conn, buf, sizeof(buf), 0);
        CHECK(n == (ssize_t)MSGLEN, "recv from IPv4-mapped client: correct length");
        CHECK(memcmp(buf, MSG, MSGLEN) == 0, "recv from IPv4-mapped client: correct data");

        /* getpeername 也应返回 IPv4-mapped 地址 */
        struct sockaddr_in6 remote = {0};
        socklen_t rlen = sizeof(remote);
        CHECK_RET(getpeername(conn, (struct sockaddr *)&remote, &rlen),
                  0, "getpeername on dual-stack conn");
        CHECK(IN6_IS_ADDR_V4MAPPED(&remote.sin6_addr),
              "getpeername: IPv4-mapped address");

        close(conn);
    }

    close(server);
    wait_child(pid, "dual-stack: AF_INET client connect + send succeeded");
}

/* ── C. IPv6-only 客户端连接 IPv6-only 服务端 ───────────────────── */
static void test_v6only_pure_ipv6(void)
{
    int server = socket(AF_INET6, SOCK_STREAM, 0);
    if (server < 0) { __fail++; return; }

    int one = 1;
    setsockopt(server, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
    setsockopt(server, IPPROTO_IPV6, IPV6_V6ONLY, &one, sizeof(one));

    struct sockaddr_in6 saddr = {0};
    saddr.sin6_family = AF_INET6;
    saddr.sin6_port   = htons(BASE_PORT + 2);
    saddr.sin6_addr   = in6addr_loopback; /* ::1 */

    if (bind(server, (struct sockaddr *)&saddr, sizeof(saddr)) != 0 ||
        listen(server, 5) != 0) {
        close(server);
        __fail++;
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        close(server);
        /* AF_INET6 客户端连接 ::1 */
        int c = socket(AF_INET6, SOCK_STREAM, 0);
        if (c < 0) _exit(1);
        if (connect(c, (struct sockaddr *)&saddr, sizeof(saddr)) != 0) {
            close(c); _exit(1);
        }
        ssize_t n = send(c, MSG, MSGLEN, 0);
        close(c);
        _exit(n == (ssize_t)MSGLEN ? 0 : 1);
    }

    struct sockaddr_in6 peer = {0};
    socklen_t plen = sizeof(peer);
    int conn = accept(server, (struct sockaddr *)&peer, &plen);
    CHECK(conn >= 0, "IPV6_V6ONLY=1: pure IPv6 client accepted");

    if (conn >= 0) {
        CHECK(!IN6_IS_ADDR_V4MAPPED(&peer.sin6_addr),
              "pure IPv6 peer is NOT IPv4-mapped");

        char buf[32] = {0};
        ssize_t n = recv(conn, buf, sizeof(buf), 0);
        CHECK(n == (ssize_t)MSGLEN, "pure IPv6: recv correct length");
        close(conn);
    }

    close(server);
    wait_child(pid, "IPV6_V6ONLY=1: pure IPv6 connect + send succeeded");
}

int main(void)
{
    TEST_START("AF_INET6 dual-stack (IPV6_V6ONLY)");

    test_v6only_rejects_v4();
    test_dualstack_accepts_v4();
    test_v6only_pure_ipv6();

    TEST_DONE();
}
