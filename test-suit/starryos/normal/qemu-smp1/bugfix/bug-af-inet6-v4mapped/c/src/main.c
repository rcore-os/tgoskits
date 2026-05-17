/*
 * bug-af-inet6-v4mapped: AF_INET6 sockets are backed by the IPv4 stack.
 *
 * StarryOS currently has a v4-only network stack.  IPv6 sockets still need
 * to accept loopback/v4-mapped endpoints used by portable user programs and
 * report IPv4 endpoints as IPv4-mapped sockaddr_in6 values.
 */

#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

static int test_passed;
static int test_failed;

#define TEST_START(name)                                                      \
    do {                                                                      \
        printf("[TEST] %s\n", (name));                                        \
        test_passed = 0;                                                      \
        test_failed = 0;                                                      \
    } while (0)

#define CHECK(cond, msg)                                                      \
    do {                                                                      \
        if (cond) {                                                           \
            printf("  [OK] %s\n", (msg));                                    \
            test_passed++;                                                    \
        } else {                                                              \
            printf("  [FAIL] %s (errno=%d %s)\n", (msg), errno,              \
                   strerror(errno));                                          \
            test_failed++;                                                    \
        }                                                                     \
    } while (0)

#define TEST_DONE()                                                           \
    do {                                                                      \
        printf("\n=== result: %d passed, %d failed ===\n", test_passed,       \
               test_failed);                                                  \
        if (test_failed == 0) {                                                \
            printf("STARRY_GROUPED_TEST_PASSED: "                            \
                   "bug-af-inet6-v4mapped\n");                               \
        } else {                                                              \
            printf("STARRY_GROUPED_TEST_FAILED: "                            \
                   "bug-af-inet6-v4mapped\n");                               \
        }                                                                     \
        return test_failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;                \
    } while (0)

static struct sockaddr_in6 in6_addr_loopback(unsigned short port)
{
    struct sockaddr_in6 addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin6_family = AF_INET6;
    addr.sin6_addr = in6addr_loopback;
    addr.sin6_port = htons(port);
    return addr;
}

static struct sockaddr_in6 in6_addr_v4mapped_loopback(unsigned short port)
{
    struct sockaddr_in6 addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin6_family = AF_INET6;
    addr.sin6_port = htons(port);
    addr.sin6_addr.s6_addr[10] = 0xff;
    addr.sin6_addr.s6_addr[11] = 0xff;
    addr.sin6_addr.s6_addr[12] = 127;
    addr.sin6_addr.s6_addr[13] = 0;
    addr.sin6_addr.s6_addr[14] = 0;
    addr.sin6_addr.s6_addr[15] = 1;
    return addr;
}

static int is_v4mapped_loopback(const struct in6_addr *addr)
{
    static const unsigned char expected[16] = {
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 127, 0, 0, 1,
    };
    return memcmp(addr->s6_addr, expected, sizeof(expected)) == 0;
}

static unsigned short get_bound_port(int fd)
{
    struct sockaddr_storage storage;
    socklen_t len = sizeof(storage);
    memset(&storage, 0, sizeof(storage));

    errno = 0;
    int rc = getsockname(fd, (struct sockaddr *)&storage, &len);
    CHECK(rc == 0, "getsockname on AF_INET6 socket");
    CHECK(storage.ss_family == AF_INET6, "getsockname reports AF_INET6");

    struct sockaddr_in6 *addr = (struct sockaddr_in6 *)&storage;
    CHECK(is_v4mapped_loopback(&addr->sin6_addr),
          "getsockname returns IPv4-mapped loopback");
    CHECK(ntohs(addr->sin6_port) != 0, "getsockname reports an ephemeral port");
    return ntohs(addr->sin6_port);
}

static void check_v4mapped_loopback_sockaddr(const struct sockaddr_storage *storage,
                                             const char *family_msg,
                                             const char *addr_msg)
{
    CHECK(storage->ss_family == AF_INET6, family_msg);
    struct sockaddr_in6 *addr = (struct sockaddr_in6 *)storage;
    CHECK(is_v4mapped_loopback(&addr->sin6_addr), addr_msg);
}

static void test_tcp_accept_v4mapped_peer(void)
{
    int listener = socket(AF_INET6, SOCK_STREAM, IPPROTO_TCP);
    CHECK(listener >= 0, "create AF_INET6 TCP listener");
    if (listener < 0) {
        return;
    }

    struct sockaddr_in6 loopback = in6_addr_loopback(0);
    errno = 0;
    int rc = bind(listener, (struct sockaddr *)&loopback, sizeof(loopback));
    CHECK(rc == 0, "bind AF_INET6 TCP listener to ::1");
    if (rc != 0) {
        close(listener);
        return;
    }
    unsigned short port = get_bound_port(listener);

    errno = 0;
    rc = listen(listener, 1);
    CHECK(rc == 0, "listen on AF_INET6 TCP socket");
    if (rc != 0) {
        close(listener);
        return;
    }

    int client = socket(AF_INET6, SOCK_STREAM, IPPROTO_TCP);
    CHECK(client >= 0, "create AF_INET6 TCP client");
    if (client < 0) {
        close(listener);
        return;
    }

    struct sockaddr_in6 mapped = in6_addr_v4mapped_loopback(port);
    errno = 0;
    rc = connect(client, (struct sockaddr *)&mapped, sizeof(mapped));
    CHECK(rc == 0, "connect AF_INET6 TCP client to ::ffff:127.0.0.1");
    if (rc != 0) {
        close(client);
        close(listener);
        return;
    }

    struct sockaddr_storage accepted_peer;
    socklen_t accepted_peer_len = sizeof(accepted_peer);
    memset(&accepted_peer, 0, sizeof(accepted_peer));
    errno = 0;
    int accepted =
        accept(listener, (struct sockaddr *)&accepted_peer, &accepted_peer_len);
    CHECK(accepted >= 0, "accept AF_INET6 TCP connection");
    if (accepted >= 0) {
        check_v4mapped_loopback_sockaddr(
            &accepted_peer, "accept reports AF_INET6 peer",
            "accept returns IPv4-mapped loopback peer");

        struct sockaddr_storage peer_storage;
        socklen_t peer_len = sizeof(peer_storage);
        memset(&peer_storage, 0, sizeof(peer_storage));
        errno = 0;
        CHECK(getpeername(accepted, (struct sockaddr *)&peer_storage, &peer_len) == 0,
              "getpeername on accepted AF_INET6 TCP socket");
        check_v4mapped_loopback_sockaddr(
            &peer_storage, "accepted getpeername reports AF_INET6",
            "accepted getpeername returns IPv4-mapped loopback");
        close(accepted);
    }

    close(client);
    close(listener);
}

int main(void)
{
    TEST_START("AF_INET6 IPv4-mapped compatibility");

    int tcp_fd = socket(AF_INET6, SOCK_STREAM, IPPROTO_TCP);
    CHECK(tcp_fd >= 0, "create AF_INET6 TCP socket");
    if (tcp_fd >= 0) {
        close(tcp_fd);
    }

    int udp_fd = socket(AF_INET6, SOCK_DGRAM, 0);
    CHECK(udp_fd >= 0, "create AF_INET6 UDP socket");
    if (udp_fd < 0) {
        TEST_DONE();
    }

    int one = 1;
    errno = 0;
    CHECK(setsockopt(udp_fd, IPPROTO_IPV6, IPV6_V6ONLY, &one, sizeof(one)) == 0,
          "accept IPV6_V6ONLY setsockopt");

    int value = -1;
    socklen_t value_len = sizeof(value);
    errno = 0;
    CHECK(getsockopt(udp_fd, IPPROTO_IPV6, IPV6_V6ONLY, &value, &value_len) == 0 &&
              value_len == sizeof(value) && value == 0,
          "IPV6_V6ONLY getsockopt reports dual-stack compatibility");

    struct sockaddr_in6 loopback = in6_addr_loopback(0);
    errno = 0;
    CHECK(bind(udp_fd, (struct sockaddr *)&loopback, sizeof(loopback)) == 0,
          "bind AF_INET6 UDP socket to ::1");
    unsigned short port = get_bound_port(udp_fd);

    int peer_fd = socket(AF_INET6, SOCK_DGRAM, 0);
    CHECK(peer_fd >= 0, "create peer AF_INET6 UDP socket");
    if (peer_fd >= 0) {
        struct sockaddr_in6 mapped = in6_addr_v4mapped_loopback(port);
        errno = 0;
        CHECK(connect(peer_fd, (struct sockaddr *)&mapped, sizeof(mapped)) == 0,
              "connect AF_INET6 UDP socket to ::ffff:127.0.0.1");

        struct sockaddr_storage peer_storage;
        socklen_t peer_len = sizeof(peer_storage);
        memset(&peer_storage, 0, sizeof(peer_storage));
        errno = 0;
        CHECK(getpeername(peer_fd, (struct sockaddr *)&peer_storage, &peer_len) == 0,
              "getpeername on mapped AF_INET6 peer");
        CHECK(peer_storage.ss_family == AF_INET6, "getpeername reports AF_INET6");
        struct sockaddr_in6 *peer = (struct sockaddr_in6 *)&peer_storage;
        CHECK(is_v4mapped_loopback(&peer->sin6_addr),
              "getpeername returns IPv4-mapped loopback");
        CHECK(ntohs(peer->sin6_port) == port, "getpeername preserves peer port");
        close(peer_fd);
    }

    close(udp_fd);
    test_tcp_accept_v4mapped_peer();
    TEST_DONE();
}
