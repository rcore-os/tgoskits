/*
 * test-unix-scm-rights
 *
 * Verifies that AF_UNIX SOCK_STREAM passes a file descriptor via
 * SCM_RIGHTS. Sender writes a known byte to a pipe, attaches the read
 * end as cmsg over a socketpair, and the receiver reads the fd back
 * out of the cmsg and reads the same byte from the dup'd pipe end.
 */

#include "test_framework.h"
#include <sys/socket.h>
#include <sys/uio.h>
#include <unistd.h>
#include <string.h>
#include <netinet/in.h>

/*
 * A UDP socket's release waits for the network poller, so it is a file
 * whose final close must not happen in atomic kernel context. Binding it
 * to a loopback port makes that final close observable: the port stays
 * busy exactly as long as some reference to the socket is still open.
 */
static int bound_udp_socket(unsigned short *port)
{
    struct sockaddr_in addr;
    socklen_t len = sizeof(addr);
    int fd = socket(AF_INET, SOCK_DGRAM, 0);

    if (fd < 0) {
        return -1;
    }
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0 ||
        getsockname(fd, (struct sockaddr *)&addr, &len) != 0) {
        close(fd);
        return -1;
    }
    *port = ntohs(addr.sin_port);
    return fd;
}

static int udp_port_in_use(unsigned short port)
{
    struct sockaddr_in addr;
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    int in_use;

    if (fd < 0) {
        return -1;
    }
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr.sin_port = htons(port);
    in_use = bind(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0 && errno == EADDRINUSE;
    close(fd);
    return in_use;
}

static ssize_t send_byte_with_fd(int sock, char byte, int fd)
{
    struct iovec iov = { .iov_base = &byte, .iov_len = 1 };
    char cbuf[CMSG_SPACE(sizeof(int))];
    struct msghdr mh = {0};

    memset(cbuf, 0, sizeof(cbuf));
    mh.msg_iov = &iov;
    mh.msg_iovlen = 1;
    mh.msg_control = cbuf;
    mh.msg_controllen = sizeof(cbuf);
    struct cmsghdr *cmh = CMSG_FIRSTHDR(&mh);
    cmh->cmsg_level = SOL_SOCKET;
    cmh->cmsg_type = SCM_RIGHTS;
    cmh->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cmh), &fd, sizeof(int));
    return sendmsg(sock, &mh, 0);
}

static size_t cmsg_aligned_len(size_t len)
{
    size_t align = sizeof(size_t) - 1;
    return (len + align) & ~align;
}

static struct cmsghdr *next_cmsg_checked(const struct msghdr *msg,
                                         const struct cmsghdr *cmsg)
{
    const unsigned char *control = (const unsigned char *)msg->msg_control;
    size_t controllen = msg->msg_controllen;
    const unsigned char *next = (const unsigned char *)cmsg + cmsg_aligned_len(cmsg->cmsg_len);
    size_t offset = (size_t)(next - control);

    if (offset > controllen || controllen - offset < sizeof(struct cmsghdr)) {
        return NULL;
    }

    struct cmsghdr *candidate = (struct cmsghdr *)(void *)next;
    if (candidate->cmsg_len < CMSG_LEN(0) || candidate->cmsg_len > controllen - offset) {
        return NULL;
    }
    return candidate;
}

/*
 * After the receiver has closed, the sender's connected channel is the
 * only owner of datagrams still queued for it. Closing the sender drops
 * that channel, so the final close of a queued SCM_RIGHTS file happens
 * inside close(2), which must still succeed and leave the kernel running.
 */
static void queued_rights_released_by_datagram_close(void)
{
    int sv[2];
    unsigned short port = 0;

    CHECK_RET(socketpair(AF_UNIX, SOCK_DGRAM, 0, sv), 0, "datagram socketpair for close release");
    int udp = bound_udp_socket(&port);
    CHECK(udp >= 0, "bind UDP socket for datagram close release");
    if (udp < 0) {
        close(sv[0]);
        close(sv[1]);
        return;
    }
    CHECK_RET(send_byte_with_fd(sv[0], 'D', udp), 1, "queue UDP fd on datagram socket");
    CHECK_RET(close(udp), 0, "close sender copy of queued UDP fd");
    CHECK_RET(close(sv[1]), 0, "close receiver without receiving the rights");
    CHECK(udp_port_in_use(port) == 1, "queued datagram keeps the UDP socket open");
    CHECK_RET(close(sv[0]), 0, "close sender releases the connected channel");
    CHECK(udp_port_in_use(port) == 0, "datagram close dropped the last UDP reference");
}

static int read_marker_from_fd(int fd, char expected, const char *msg)
{
    char got = 0;
    ssize_t ret = read(fd, &got, 1);
    CHECK_RET(ret, 1, msg);
    CHECK(got == expected, "received fd reads expected marker");
    return ret == 1 && got == expected;
}

int main(void)
{
    TEST_START("AF_UNIX SOCK_STREAM SCM_RIGHTS round-trip");

    queued_rights_released_by_datagram_close();

    int sv[2];
    CHECK_RET(socketpair(AF_UNIX, SOCK_STREAM, 0, sv), 0, "socketpair");

    int p[2];
    CHECK_RET(pipe(p), 0, "pipe");
    char marker = 'Z';
    CHECK_RET(write(p[1], &marker, 1), 1, "write 1 byte to pipe");

    /* sendmsg with SCM_RIGHTS attaching p[0] as cmsg. */
    char payload = 'A';
    struct iovec iov = { .iov_base = &payload, .iov_len = 1 };
    char cbuf[CMSG_SPACE(sizeof(int))];
    memset(cbuf, 0, sizeof(cbuf));
    struct msghdr mh = {0};
    mh.msg_iov = &iov;
    mh.msg_iovlen = 1;
    mh.msg_control = cbuf;
    mh.msg_controllen = sizeof(cbuf);
    struct cmsghdr *cmh = CMSG_FIRSTHDR(&mh);
    cmh->cmsg_level = SOL_SOCKET;
    cmh->cmsg_type = SCM_RIGHTS;
    cmh->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cmh), &p[0], sizeof(int));
    mh.msg_controllen = sizeof(cbuf);

    ssize_t s = sendmsg(sv[0], &mh, 0);
    CHECK_RET(s, 1, "sendmsg with SCM_RIGHTS");

    /* recvmsg, expect payload byte and a cmsg carrying an fd. */
    char rxbuf = 0;
    struct iovec riov = { .iov_base = &rxbuf, .iov_len = 1 };
    char rcbuf[CMSG_SPACE(sizeof(int))];
    memset(rcbuf, 0, sizeof(rcbuf));
    struct msghdr rmh = {0};
    rmh.msg_iov = &riov;
    rmh.msg_iovlen = 1;
    rmh.msg_control = rcbuf;
    rmh.msg_controllen = sizeof(rcbuf);

    ssize_t r = recvmsg(sv[1], &rmh, 0);
    CHECK_RET(r, 1, "recvmsg returns 1 byte");
    CHECK(rxbuf == payload, "payload byte matches");

    int got_fd = -1;
    struct cmsghdr *rcmh = CMSG_FIRSTHDR(&rmh);
    CHECK(rcmh != NULL, "received a cmsg");
    if (rcmh) {
        CHECK(rcmh->cmsg_level == SOL_SOCKET, "cmsg_level == SOL_SOCKET");
        CHECK(rcmh->cmsg_type == SCM_RIGHTS, "cmsg_type == SCM_RIGHTS");
        memcpy(&got_fd, CMSG_DATA(rcmh), sizeof(int));
        CHECK(got_fd >= 0, "received fd >= 0");
    }

    /* Read the marker byte through the dup'd pipe end on the receive
     * side. If SCM_RIGHTS didn't actually pass the fd, this will fail. */
    if (got_fd >= 0) {
        read_marker_from_fd(got_fd, marker, "read 1 byte from received pipe fd");
        close(got_fd);
    }

    close(p[0]);
    close(p[1]);
    close(sv[0]);
    close(sv[1]);

    CHECK_RET(socketpair(AF_UNIX, SOCK_STREAM, 0, sv), 0, "socketpair for two cmsgs");

    int p1[2];
    int p2[2];
    CHECK_RET(pipe(p1), 0, "pipe #1");
    CHECK_RET(pipe(p2), 0, "pipe #2");
    CHECK_RET(write(p1[1], "L", 1), 1, "write marker L");
    CHECK_RET(write(p2[1], "R", 1), 1, "write marker R");

    char payload2 = 'B';
    struct iovec iov2 = { .iov_base = &payload2, .iov_len = 1 };
    char cbuf2[CMSG_SPACE(sizeof(int)) * 2];
    memset(cbuf2, 0, sizeof(cbuf2));
    struct msghdr mh2 = {0};
    mh2.msg_iov = &iov2;
    mh2.msg_iovlen = 1;
    mh2.msg_control = cbuf2;
    mh2.msg_controllen = sizeof(cbuf2);

    struct cmsghdr *first = CMSG_FIRSTHDR(&mh2);
    CHECK(first != NULL, "first sender cmsg available");
    first->cmsg_level = SOL_SOCKET;
    first->cmsg_type = SCM_RIGHTS;
    first->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(first), &p1[0], sizeof(int));

    struct cmsghdr *second =
        (struct cmsghdr *)((char *)first + CMSG_SPACE(sizeof(int)));
    second->cmsg_level = SOL_SOCKET;
    second->cmsg_type = SCM_RIGHTS;
    second->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(second), &p2[0], sizeof(int));

    mh2.msg_controllen = CMSG_SPACE(sizeof(int)) + CMSG_LEN(sizeof(int));
    CHECK_RET(sendmsg(sv[0], &mh2, 0), 1, "sendmsg with two aligned SCM_RIGHTS cmsgs");

    char rxbuf2 = 0;
    struct iovec riov2 = { .iov_base = &rxbuf2, .iov_len = 1 };
    char rcbuf2[CMSG_SPACE(sizeof(int) * 2) + CMSG_SPACE(sizeof(int))];
    memset(rcbuf2, 0, sizeof(rcbuf2));
    struct msghdr rmh2 = {0};
    rmh2.msg_iov = &riov2;
    rmh2.msg_iovlen = 1;
    rmh2.msg_control = rcbuf2;
    rmh2.msg_controllen = sizeof(rcbuf2);

    CHECK_RET(recvmsg(sv[1], &rmh2, 0), 1, "recvmsg with two SCM_RIGHTS cmsgs");
    CHECK(rxbuf2 == payload2, "payload byte for two-cmsg message matches");

    int received[4] = {-1, -1, -1, -1};
    int received_count = 0;
    for (struct cmsghdr *cmsg = CMSG_FIRSTHDR(&rmh2);
         cmsg != NULL;
         cmsg = next_cmsg_checked(&rmh2, cmsg)) {
        if (cmsg->cmsg_level != SOL_SOCKET || cmsg->cmsg_type != SCM_RIGHTS) {
            continue;
        }
        size_t data_len = cmsg->cmsg_len - CMSG_LEN(0);
        int fd_count = (int)(data_len / sizeof(int));
        int *fds = (int *)CMSG_DATA(cmsg);
        for (int i = 0; i < fd_count && received_count < 4; i++) {
            received[received_count++] = fds[i];
        }
    }
    CHECK(received_count == 2, "received exactly two SCM_RIGHTS fds");
    if (received_count == 2) {
        int left_ok = read_marker_from_fd(received[0], 'L', "read marker from first received fd");
        int right_ok = read_marker_from_fd(received[1], 'R', "read marker from second received fd");
        CHECK(left_ok && right_ok, "two aligned cmsgs preserve fd order and contents");
    }
    for (int i = 0; i < received_count; i++) {
        close(received[i]);
    }
    close(p1[0]);
    close(p1[1]);
    close(p2[0]);
    close(p2[1]);
    close(sv[0]);
    close(sv[1]);

    TEST_DONE();
}
