#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "test_framework.h"
#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/ip.h>
#include <netinet/ip_icmp.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static unsigned short checksum(const void *data, size_t len) {
    const unsigned short *words = data;
    unsigned int sum = 0;

    while (len > 1) {
        sum += *words++;
        len -= 2;
    }
    if (len == 1) {
        sum += *(const unsigned char *)words;
    }
    while (sum >> 16) {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    return (unsigned short)~sum;
}

static struct sockaddr_in loopback_addr(const char *ip) {
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    CHECK(inet_pton(AF_INET, ip, &addr.sin_addr) == 1, "inet_pton succeeds");
    return addr;
}

static int make_raw_socket(void) {
    int fd = socket(AF_INET, SOCK_RAW | SOCK_NONBLOCK, IPPROTO_ICMP);
    CHECK(fd >= 0, "create nonblocking raw ICMP socket");
    return fd;
}

static int run_in_child(void (*func)(void)) {
    pid_t pid = fork();
    if (pid < 0) {
        return 0;
    }
    if (pid == 0) {
        func();
        _exit(__fail > 0 ? 1 : 0);
    }

    int status = 0;
    pid_t waited;
    do {
        waited = waitpid(pid, &status, 0);
    } while (waited == -1 && errno == EINTR);
    if (waited == -1) {
        return 0;
    }
    return WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

static void child_raw_socket_requires_root(void) {
    CHECK_RET(syscall(SYS_setresuid, 1000, 1000, 1000), 0,
              "drop root credentials");

    errno = 0;
    int fd = socket(AF_INET, SOCK_RAW, IPPROTO_ICMP);
    CHECK(fd == -1 && errno == EPERM, "non-root raw ICMP socket returns EPERM");
    if (fd >= 0) {
        close(fd);
    }
}

static void send_echo_request(int fd, struct sockaddr_in dst, unsigned short ident) {
    struct icmphdr icmp;
    memset(&icmp, 0, sizeof(icmp));
    icmp.type = ICMP_ECHO;
    icmp.code = 0;
    icmp.un.echo.id = htons(ident);
    icmp.un.echo.sequence = htons(1);
    icmp.checksum = checksum(&icmp, sizeof(icmp));

    CHECK_RET(sendto(fd, &icmp, sizeof(icmp), 0, (struct sockaddr *)&dst, sizeof(dst)),
              (ssize_t)sizeof(icmp), "send raw ICMP echo request");
}

static int recv_icmp_from(int fd, int flags, struct in_addr expected_src,
                          unsigned short expected_ident) {
    unsigned char buf[256];
    ssize_t n = recv(fd, buf, sizeof(buf), flags);
    if (n < 0) {
        return -1;
    }

    CHECK(n >= (ssize_t)(sizeof(struct iphdr) + sizeof(struct icmphdr)),
          "raw recv returns IPv4 header and ICMP payload");
    if (n < (ssize_t)(sizeof(struct iphdr) + sizeof(struct icmphdr))) {
        return -1;
    }

    struct iphdr *ip = (struct iphdr *)buf;
    size_t ihl = ip->ihl * 4;
    CHECK(ihl >= sizeof(struct iphdr) && n >= (ssize_t)(ihl + sizeof(struct icmphdr)),
          "IPv4 header length is valid");
    if (ihl < sizeof(struct iphdr) || n < (ssize_t)(ihl + sizeof(struct icmphdr))) {
        return -1;
    }

    struct icmphdr *icmp = (struct icmphdr *)(buf + ihl);
    CHECK(ip->saddr == expected_src.s_addr, "raw packet source matches connected peer");
    CHECK(icmp->type == ICMP_ECHO, "raw packet is ICMP echo request");
    CHECK(ntohs(icmp->un.echo.id) == expected_ident, "raw packet identifier matches");
    return 0;
}

int main(void) {
    TEST_START("raw-msg-peek");

    struct sockaddr_in actual_peer = loopback_addr("127.0.0.1");
    struct sockaddr_in wrong_peer = loopback_addr("127.0.0.2");
    const unsigned short ident = 0x2345;

    CHECK(run_in_child(child_raw_socket_requires_root),
          "raw socket creation requires root credentials");

    int recv_fd = make_raw_socket();
    int send_fd = make_raw_socket();

    CHECK_RET(connect(recv_fd, (struct sockaddr *)&wrong_peer, sizeof(wrong_peer)), 0,
              "connect receiver to alternate peer");

    send_echo_request(send_fd, actual_peer, ident);

    CHECK_ERR(recv(recv_fd, &(unsigned char){0}, 1, MSG_PEEK), EAGAIN,
              "MSG_PEEK rejects non-peer packet without consuming it");

    CHECK_RET(connect(recv_fd, (struct sockaddr *)&actual_peer, sizeof(actual_peer)), 0,
              "reconnect receiver to actual peer");
    CHECK_RET(recv_icmp_from(recv_fd, 0, actual_peer.sin_addr, ident), 0,
              "non-peer packet remains readable after MSG_PEEK");

    close(send_fd);
    close(recv_fd);
    TEST_DONE();
}
