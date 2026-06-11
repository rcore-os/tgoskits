/*
 * test-sockopt — getsockopt / setsockopt 控制面系统调用测试
 *
 * 覆盖 SO_TYPE / SO_PROTOCOL / SO_DOMAIN 三个只读 socket option。
 * 依据: Linux man-pages 7 socket(7) getsockopt(2)。
 *
 * =====================================================================
 * 手册摘要 (man 7 socket, man 2 getsockopt)
 * =====================================================================
 *
 * ── SO_TYPE ───────────────────────────────────────────────────────────
 *   int type;
 *   getsockopt(fd, SOL_SOCKET, SO_TYPE, &type, &len);
 *
 *   返回 socket 创建时的类型参数 (SOCK_STREAM=1, SOCK_DGRAM=2, SOCK_RAW=3)。
 *   只读; setsockopt 返回 ENOPROTOOPT。
 *
 * ── SO_PROTOCOL ──────────────────────────────────────────────────────
 *   int proto;
 *   getsockopt(fd, SOL_SOCKET, SO_PROTOCOL, &proto, &len);
 *
 *   返回 socket 创建时的协议号 (IPPROTO_TCP=6, IPPROTO_UDP=17, 等)。
 *   只读; setsockopt 返回 ENOPROTOOPT。
 *   Linux 3.13+ 可用。
 *
 * ── SO_DOMAIN ────────────────────────────────────────────────────────
 *   int domain;
 *   getsockopt(fd, SOL_SOCKET, SO_DOMAIN, &domain, &len);
 *
 *   返回 socket 创建时的地址族 (AF_INET=2, AF_UNIX=1, AF_VSOCK=40, 等)。
 *   只读; setsockopt 返回 ENOPROTOOPT。
 *   Linux 3.13+ 可用。
 */

#include "test_framework.h"

#include <errno.h>
#include <netinet/in.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

/* ---- SO_TYPE (3) ---- */

static void test_so_type(void) {
    TEST_START("SO_TYPE");

    int fd_tcp = socket(AF_INET, SOCK_STREAM, 0);
    int fd_udp = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(fd_tcp >= 0, "socket(TCP)");
    CHECK(fd_udp >= 0, "socket(UDP)");

    /* TCP: SO_TYPE == SOCK_STREAM (1) */
    int type = -1;
    socklen_t len = sizeof(type);
    CHECK_RET(getsockopt(fd_tcp, SOL_SOCKET, SO_TYPE, &type, &len), 0,
              "getsockopt(SO_TYPE) on TCP");
    CHECK(type == SOCK_STREAM, "TCP: SO_TYPE == SOCK_STREAM (1)");

    /* UDP: SO_TYPE == SOCK_DGRAM (2) */
    type = -1;
    len = sizeof(type);
    CHECK_RET(getsockopt(fd_udp, SOL_SOCKET, SO_TYPE, &type, &len), 0,
              "getsockopt(SO_TYPE) on UDP");
    CHECK(type == SOCK_DGRAM, "UDP: SO_TYPE == SOCK_DGRAM (2)");

    /* 只读: setsockopt 必须拒绝 */
    int val = 1;
    CHECK_ERR(setsockopt(fd_tcp, SOL_SOCKET, SO_TYPE, &val, sizeof(val)),
              ENOPROTOOPT, "setsockopt(SO_TYPE) returns ENOPROTOOPT");

    close(fd_tcp);
    close(fd_udp);
}

/* ---- SO_PROTOCOL (3) ---- */

static void test_so_protocol(void) {
    TEST_START("SO_PROTOCOL");

    int fd_tcp = socket(AF_INET, SOCK_STREAM, 0);
    int fd_udp = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(fd_tcp >= 0, "socket(TCP)");
    CHECK(fd_udp >= 0, "socket(UDP)");

    /* TCP: SO_PROTOCOL == IPPROTO_TCP (6) */
    int proto = -1;
    socklen_t len = sizeof(proto);
    CHECK_RET(getsockopt(fd_tcp, SOL_SOCKET, SO_PROTOCOL, &proto, &len), 0,
              "getsockopt(SO_PROTOCOL) on TCP");
    CHECK(proto == 6, "TCP: SO_PROTOCOL == IPPROTO_TCP (6)");

    /* UDP: SO_PROTOCOL == IPPROTO_UDP (17) */
    proto = -1;
    len = sizeof(proto);
    CHECK_RET(getsockopt(fd_udp, SOL_SOCKET, SO_PROTOCOL, &proto, &len), 0,
              "getsockopt(SO_PROTOCOL) on UDP");
    CHECK(proto == 17, "UDP: SO_PROTOCOL == IPPROTO_UDP (17)");

    /* 只读: setsockopt 必须拒绝 */
    int val = 1;
    CHECK_ERR(setsockopt(fd_tcp, SOL_SOCKET, SO_PROTOCOL, &val, sizeof(val)),
              ENOPROTOOPT, "setsockopt(SO_PROTOCOL) returns ENOPROTOOPT");

    close(fd_tcp);
    close(fd_udp);
}

/* ---- SO_DOMAIN (3) ---- */

static void test_so_domain(void) {
    TEST_START("SO_DOMAIN");

    int fd_tcp = socket(AF_INET, SOCK_STREAM, 0);
    int fd_udp = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(fd_tcp >= 0, "socket(TCP)");
    CHECK(fd_udp >= 0, "socket(UDP)");

    /* TCP: SO_DOMAIN == AF_INET (2) */
    int domain = -1;
    socklen_t len = sizeof(domain);
    CHECK_RET(getsockopt(fd_tcp, SOL_SOCKET, SO_DOMAIN, &domain, &len), 0,
              "getsockopt(SO_DOMAIN) on TCP");
    CHECK(domain == AF_INET, "TCP: SO_DOMAIN == AF_INET (2)");

    /* UDP: SO_DOMAIN == AF_INET (2) */
    domain = -1;
    len = sizeof(domain);
    CHECK_RET(getsockopt(fd_udp, SOL_SOCKET, SO_DOMAIN, &domain, &len), 0,
              "getsockopt(SO_DOMAIN) on UDP");
    CHECK(domain == AF_INET, "UDP: SO_DOMAIN == AF_INET (2)");

    /* 只读: setsockopt 必须拒绝 */
    int val = 1;
    CHECK_ERR(setsockopt(fd_tcp, SOL_SOCKET, SO_DOMAIN, &val, sizeof(val)),
              ENOPROTOOPT, "setsockopt(SO_DOMAIN) returns ENOPROTOOPT");

    close(fd_tcp);
    close(fd_udp);
}

/* ---- Main ---- */

int main(void) {
    /* P0: pip blocker */
    test_so_type();

    /* P1: protocol / domain */
    test_so_protocol();
    test_so_domain();

    TEST_DONE();
}
