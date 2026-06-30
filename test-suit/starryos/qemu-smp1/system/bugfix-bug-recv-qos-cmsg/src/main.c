#define _GNU_SOURCE

#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#ifndef IP_RECVTOS
#define IP_RECVTOS 13
#endif

#ifndef IPV6_RECVTCLASS
#define IPV6_RECVTCLASS 66
#endif

#ifndef IPV6_TCLASS
#define IPV6_TCLASS 67
#endif

struct cmsg_result {
    int has_ip_tos;
    int ip_tos;
    int has_ipv6_tclass;
    int ipv6_tclass;
};

static int passed;
static int failed;

static void note_pass(const char *name)
{
    printf("PASS: %s\n", name);
    passed++;
}

static void note_fail(const char *name, const char *detail)
{
    printf("FAIL: %s: %s\n", name, detail);
    failed++;
}

static int expect_true(int condition, const char *name)
{
    if (condition) {
        note_pass(name);
        return 1;
    }
    note_fail(name, "condition is false");
    return 0;
}

static void expect_sockopt_bool(int fd, int level, int optname, int value,
                                const char *name)
{
    errno = 0;
    if (setsockopt(fd, level, optname, &value, sizeof(value)) != 0) {
        char detail[160];
        snprintf(detail, sizeof(detail), "setsockopt errno=%d (%s)", errno,
                 strerror(errno));
        note_fail(name, detail);
        return;
    }

    int got = -1;
    socklen_t got_len = sizeof(got);
    errno = 0;
    if (getsockopt(fd, level, optname, &got, &got_len) == 0 &&
        got_len == sizeof(got) && got == (value != 0)) {
        note_pass(name);
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail), "getsockopt got=%d len=%u errno=%d (%s)",
             got, (unsigned)got_len, errno, strerror(errno));
    note_fail(name, detail);
}

static void expect_sockopt_errno(int fd, int level, int optname, int value,
                                 int expected_errno, const char *name)
{
    errno = 0;
    int ret = setsockopt(fd, level, optname, &value, sizeof(value));
    int saved_errno = errno;
    if (ret == -1 && saved_errno == expected_errno) {
        note_pass(name);
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail),
             "ret=%d errno=%d (%s), expected -1/%d", ret, saved_errno,
             strerror(saved_errno), expected_errno);
    note_fail(name, detail);
}

static int bind_udp4_loopback(int fd, struct sockaddr_in *addr)
{
    memset(addr, 0, sizeof(*addr));
    addr->sin_family = AF_INET;
    addr->sin_port = 0;
    addr->sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (bind(fd, (struct sockaddr *)addr, sizeof(*addr)) != 0) {
        return -1;
    }

    socklen_t len = sizeof(*addr);
    return getsockname(fd, (struct sockaddr *)addr, &len);
}

static int bind_udp6_loopback(int fd, struct sockaddr_in6 *addr)
{
    memset(addr, 0, sizeof(*addr));
    addr->sin6_family = AF_INET6;
    addr->sin6_port = 0;
    if (inet_pton(AF_INET6, "::1", &addr->sin6_addr) != 1) {
        return -1;
    }
    if (bind(fd, (struct sockaddr *)addr, sizeof(*addr)) != 0) {
        return -1;
    }

    socklen_t len = sizeof(*addr);
    return getsockname(fd, (struct sockaddr *)addr, &len);
}

static void collect_cmsgs(struct msghdr *msg, struct cmsg_result *result)
{
    memset(result, 0, sizeof(*result));
    struct cmsghdr *cmsg = CMSG_FIRSTHDR(msg);
    if (cmsg != NULL) {
        if (cmsg->cmsg_level == IPPROTO_IP && cmsg->cmsg_type == IP_TOS &&
            cmsg->cmsg_len >= CMSG_LEN(1)) {
            result->has_ip_tos = 1;
            result->ip_tos = *(unsigned char *)CMSG_DATA(cmsg);
        } else if (cmsg->cmsg_level == IPPROTO_IPV6 &&
                   cmsg->cmsg_type == IPV6_TCLASS &&
                   cmsg->cmsg_len >= CMSG_LEN(sizeof(int))) {
            int value = -1;
            memcpy(&value, CMSG_DATA(cmsg), sizeof(value));
            result->has_ipv6_tclass = 1;
            result->ipv6_tclass = value;
        }
    }
}

static int recv_one(int fd, const char *expected, struct cmsg_result *result)
{
    char data[32] = {0};
    char cbuf[CMSG_SPACE(sizeof(int)) + CMSG_SPACE(1)] = {0};
    struct iovec iov = {
        .iov_base = data,
        .iov_len = sizeof(data),
    };
    struct msghdr msg = {
        .msg_iov = &iov,
        .msg_iovlen = 1,
        .msg_control = cbuf,
        .msg_controllen = sizeof(cbuf),
    };

    ssize_t nread = -1;
    for (int attempt = 0; attempt < 100; attempt++) {
        errno = 0;
        nread = recvmsg(fd, &msg, MSG_DONTWAIT);
        if (nread >= 0 || (errno != EAGAIN && errno != EWOULDBLOCK)) {
            break;
        }
        usleep(10000);
    }
    if (nread != (ssize_t)strlen(expected) ||
        memcmp(data, expected, strlen(expected)) != 0) {
        char detail[180];
        snprintf(detail, sizeof(detail), "recvmsg nread=%zd errno=%d (%s)",
                 nread, errno, strerror(errno));
        note_fail("recvmsg payload", detail);
        return -1;
    }
    note_pass("recvmsg payload");
    collect_cmsgs(&msg, result);
    return 0;
}

static void test_option_toggles(void)
{
    int fd4 = socket(AF_INET, SOCK_DGRAM, 0);
    int fd6 = socket(AF_INET6, SOCK_DGRAM, 0);
    if (!expect_true(fd4 >= 0, "create AF_INET UDP socket") ||
        !expect_true(fd6 >= 0, "create AF_INET6 UDP socket")) {
        if (fd4 >= 0) {
            close(fd4);
        }
        if (fd6 >= 0) {
            close(fd6);
        }
        return;
    }

    expect_sockopt_bool(fd4, IPPROTO_IP, IP_RECVTOS, 1,
                        "IP_RECVTOS enables on AF_INET");
    expect_sockopt_bool(fd4, IPPROTO_IP, IP_RECVTOS, 0,
                        "IP_RECVTOS disables on AF_INET");
    expect_sockopt_errno(fd4, IPPROTO_IPV6, IPV6_RECVTCLASS, 1, ENOPROTOOPT,
                         "AF_INET rejects IPV6_RECVTCLASS");

    expect_sockopt_bool(fd6, IPPROTO_IP, IP_RECVTOS, 1,
                        "IP_RECVTOS enables on AF_INET6");
    expect_sockopt_bool(fd6, IPPROTO_IPV6, IPV6_RECVTCLASS, 1,
                        "IPV6_RECVTCLASS enables on AF_INET6");
    expect_sockopt_bool(fd6, IPPROTO_IPV6, IPV6_RECVTCLASS, 0,
                        "IPV6_RECVTCLASS disables on AF_INET6");

    close(fd4);
    close(fd6);
}

static void test_ipv4_recvtos_cmsg(void)
{
    int rx = socket(AF_INET, SOCK_DGRAM, 0);
    int tx = socket(AF_INET, SOCK_DGRAM, 0);
    if (!expect_true(rx >= 0 && tx >= 0, "create IPv4 UDP pair")) {
        if (rx >= 0) {
            close(rx);
        }
        if (tx >= 0) {
            close(tx);
        }
        return;
    }

    struct sockaddr_in addr;
    if (!expect_true(bind_udp4_loopback(rx, &addr) == 0,
                     "bind IPv4 UDP receiver to loopback")) {
        close(rx);
        close(tx);
        return;
    }

    int tos = 0x2e;
    expect_true(setsockopt(tx, IPPROTO_IP, IP_TOS, &tos, sizeof(tos)) == 0,
                "set sender IP_TOS");

    const char first[] = "no-cmsg";
    expect_true(sendto(tx, first, strlen(first), 0, (struct sockaddr *)&addr,
                       sizeof(addr)) == (ssize_t)strlen(first),
                "send IPv4 datagram before IP_RECVTOS");
    struct cmsg_result result;
    if (recv_one(rx, first, &result) == 0) {
        expect_true(!result.has_ip_tos && !result.has_ipv6_tclass,
                    "IP_RECVTOS disabled returns no QoS cmsg");
    }

    int one = 1;
    expect_true(setsockopt(rx, IPPROTO_IP, IP_RECVTOS, &one, sizeof(one)) == 0,
                "enable receiver IP_RECVTOS");
    const char second[] = "ip-tos";
    expect_true(sendto(tx, second, strlen(second), 0, (struct sockaddr *)&addr,
                       sizeof(addr)) == (ssize_t)strlen(second),
                "send IPv4 datagram with IP_RECVTOS");
    if (recv_one(rx, second, &result) == 0) {
        expect_true(result.has_ip_tos && result.ip_tos == 0x2c,
                    "recvmsg reports IP_TOS cmsg");
        expect_true(!result.has_ipv6_tclass,
                    "IPv4 IP_RECVTOS does not report IPV6_TCLASS");
    }

    close(rx);
    close(tx);
}

static void test_ipv6_recvtclass_cmsg(void)
{
    int rx = socket(AF_INET6, SOCK_DGRAM, 0);
    int tx = socket(AF_INET6, SOCK_DGRAM, 0);
    if (!expect_true(rx >= 0 && tx >= 0, "create AF_INET6 UDP pair")) {
        if (rx >= 0) {
            close(rx);
        }
        if (tx >= 0) {
            close(tx);
        }
        return;
    }

    struct sockaddr_in6 bind_addr;
    if (!expect_true(bind_udp6_loopback(rx, &bind_addr) == 0,
                     "bind AF_INET6 UDP receiver to ::1")) {
        close(rx);
        close(tx);
        return;
    }

    struct sockaddr_in6 dst = bind_addr;
    if (inet_pton(AF_INET6, "::ffff:127.0.0.1", &dst.sin6_addr) != 1) {
        note_fail("build IPv4-mapped destination", "inet_pton failed");
        close(rx);
        close(tx);
        return;
    }

    int tclass = 0x2e;
    expect_true(setsockopt(tx, IPPROTO_IPV6, IPV6_TCLASS, &tclass,
                           sizeof(tclass)) == 0,
                "set sender IPV6_TCLASS");

    int one = 1;
    expect_true(setsockopt(rx, IPPROTO_IPV6, IPV6_RECVTCLASS, &one,
                           sizeof(one)) == 0,
                "enable receiver IPV6_RECVTCLASS");

    const char payload[] = "ipv6-tclass";
    expect_true(sendto(tx, payload, strlen(payload), 0, (struct sockaddr *)&dst,
                       sizeof(dst)) == (ssize_t)strlen(payload),
                "send AF_INET6 datagram through mapped loopback");

    struct cmsg_result result;
    if (recv_one(rx, payload, &result) == 0) {
        expect_true(result.has_ipv6_tclass && result.ipv6_tclass == 0x2c,
                    "recvmsg reports IPV6_TCLASS cmsg");
        expect_true(!result.has_ip_tos,
                    "IPV6_RECVTCLASS does not report IP_TOS cmsg");
    }

    close(rx);
    close(tx);
}

int main(void)
{
    printf("=== bug-recv-qos-cmsg ===\n");

    test_option_toggles();
    test_ipv4_recvtos_cmsg();
    test_ipv6_recvtclass_cmsg();

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("STARRY_GROUPED_TEST_PASSED: bug-recv-qos-cmsg\n");
        return EXIT_SUCCESS;
    }
    printf("STARRY_GROUPED_TEST_FAILED: bug-recv-qos-cmsg\n");
    return EXIT_FAILURE;
}
