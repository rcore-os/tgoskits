/*
 * bug-tcp-udp-bind-same-port: TCP and UDP use separate port spaces.
 *
 * Regression: TCP bind incorrectly consulted the UDP socket set and rejected
 * a TCP bind when a UDP socket was already bound to the same local endpoint.
 *
 * The expected visible behavior is:
 *   - TCP and UDP may bind the same local IP:port.
 *   - Two TCP sockets cannot bind the same local IP:port.
 *   - Two UDP sockets cannot bind the same local IP:port.
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
                   "bug-tcp-udp-bind-same-port\n");                          \
        } else {                                                              \
            printf("STARRY_GROUPED_TEST_FAILED: "                            \
                   "bug-tcp-udp-bind-same-port\n");                          \
        }                                                                     \
        return test_failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;                \
    } while (0)

static struct sockaddr_in loopback_addr(unsigned short port)
{
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr.sin_port = htons(port);
    return addr;
}

static unsigned short bound_port(int fd)
{
    struct sockaddr_in addr;
    socklen_t len = sizeof(addr);
    if (getsockname(fd, (struct sockaddr *)&addr, &len) != 0) {
        fprintf(stderr, "getsockname: %s\n", strerror(errno));
        exit(EXIT_FAILURE);
    }
    return ntohs(addr.sin_port);
}

static int bind_socket(int domain, int type, unsigned short port)
{
    int fd = socket(domain, type, 0);
    if (fd < 0) {
        fprintf(stderr, "socket(type=%d): %s\n", type, strerror(errno));
        exit(EXIT_FAILURE);
    }

    struct sockaddr_in addr = loopback_addr(port);
    if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
        int err = errno;
        close(fd);
        errno = err;
        return -1;
    }

    return fd;
}

static void expect_bind_ok(int domain, int type, unsigned short port,
                           const char *label)
{
    errno = 0;
    int fd = bind_socket(domain, type, port);
    CHECK(fd >= 0, label);
    if (fd >= 0) {
        close(fd);
    }
}

static void expect_bind_eaddrinuse(int domain, int type, unsigned short port,
                                   const char *label)
{
    errno = 0;
    int fd = bind_socket(domain, type, port);
    int saved_errno = errno;
    CHECK(fd < 0 && saved_errno == EADDRINUSE, label);
    if (fd >= 0) {
        close(fd);
    }
}

int main(void)
{
    TEST_START("TCP and UDP bind same local port independently");

    int udp_fd = bind_socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(udp_fd >= 0, "bind UDP loopback ephemeral port");
    unsigned short udp_port = bound_port(udp_fd);
    expect_bind_ok(AF_INET, SOCK_STREAM, udp_port,
                   "bind TCP to existing UDP local port");
    close(udp_fd);

    int tcp_fd = bind_socket(AF_INET, SOCK_STREAM, 0);
    CHECK(tcp_fd >= 0, "bind TCP loopback ephemeral port");
    unsigned short tcp_port = bound_port(tcp_fd);
    expect_bind_ok(AF_INET, SOCK_DGRAM, tcp_port,
                   "bind UDP to existing TCP local port");
    close(tcp_fd);

    int first_tcp = bind_socket(AF_INET, SOCK_STREAM, 0);
    CHECK(first_tcp >= 0, "bind first TCP socket");
    unsigned short duplicate_tcp_port = bound_port(first_tcp);
    expect_bind_eaddrinuse(AF_INET, SOCK_STREAM, duplicate_tcp_port,
                           "duplicate TCP bind returns EADDRINUSE");
    close(first_tcp);

    int first_udp = bind_socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(first_udp >= 0, "bind first UDP socket");
    unsigned short duplicate_udp_port = bound_port(first_udp);
    expect_bind_eaddrinuse(AF_INET, SOCK_DGRAM, duplicate_udp_port,
                           "duplicate UDP bind returns EADDRINUSE");
    close(first_udp);

    TEST_DONE();
}
