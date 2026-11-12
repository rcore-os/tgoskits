/*
 * test-dontwait
 *
 * Verifies that recvmsg(MSG_DONTWAIT) on an empty *blocking* socket
 * returns EAGAIN immediately across multiple transports. Linux
 * implements MSG_DONTWAIT as a per-syscall override of the blocking
 * disposition; the kernel must respect it whether the underlying
 * transport is AF_UNIX stream, AF_UNIX datagram, or AF_INET TCP.
 * Previous implementations only honored the flag on the Unix-stream
 * path; this test exercises each transport so a future regression
 * cannot silently re-introduce the bug.
 */

#include "test_framework.h"
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <unistd.h>

#ifndef MSG_DONTWAIT
#define MSG_DONTWAIT 0x40
#endif

static int is_blocking(int fd) {
    int fl = fcntl(fd, F_GETFL, 0);
    return fl != -1 && (fl & O_NONBLOCK) == 0;
}

static void check_recvmsg_eagain(int fd, const char *transport) {
    char buf[8];
    struct iovec iov = { .iov_base = buf, .iov_len = sizeof(buf) };
    struct msghdr mh = {0};
    mh.msg_iov = &iov;
    mh.msg_iovlen = 1;

    CHECK(is_blocking(fd), "socket is blocking");
    errno = 0;
    ssize_t r = recvmsg(fd, &mh, MSG_DONTWAIT);
    CHECK(r < 0, "recvmsg(MSG_DONTWAIT) on empty socket returns -1");
    CHECK(errno == EAGAIN || errno == EWOULDBLOCK,
          "errno is EAGAIN/EWOULDBLOCK");
    CHECK(is_blocking(fd),
          "socket still blocking after MSG_DONTWAIT recv");
    (void)transport;
}

int main(void)
{
    TEST_START("MSG_DONTWAIT across transports");

    /* --- AF_UNIX SOCK_STREAM via socketpair --- */
    {
        int sv[2];
        CHECK_RET(socketpair(AF_UNIX, SOCK_STREAM, 0, sv), 0, "socketpair");
        check_recvmsg_eagain(sv[1], "unix stream");
        close(sv[0]);
        close(sv[1]);
    }

    /* --- AF_UNIX SOCK_DGRAM via socketpair --- */
    {
        int sv[2];
        CHECK_RET(socketpair(AF_UNIX, SOCK_DGRAM, 0, sv), 0, "socketpair dgram");
        check_recvmsg_eagain(sv[1], "unix dgram");
        close(sv[0]);
        close(sv[1]);
    }

    /* --- AF_INET SOCK_STREAM (TCP) on loopback --- */
    {
        int listen_fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(listen_fd >= 0, "tcp listen socket created");
        if (listen_fd < 0) {
            TEST_DONE();
        }

        int one = 1;
        (void)setsockopt(listen_fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));

        struct sockaddr_in addr = {
            .sin_family = AF_INET,
            .sin_port = 0,
            .sin_addr.s_addr = htonl(INADDR_LOOPBACK),
        };
        CHECK_RET(bind(listen_fd, (struct sockaddr *)&addr, sizeof(addr)), 0,
                  "tcp bind loopback");
        socklen_t alen = sizeof(addr);
        CHECK_RET(getsockname(listen_fd, (struct sockaddr *)&addr, &alen), 0,
                  "tcp getsockname");
        CHECK_RET(listen(listen_fd, 4), 0, "tcp listen");

        int cli = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(cli >= 0, "tcp client socket");
        CHECK_RET(connect(cli, (struct sockaddr *)&addr, sizeof(addr)), 0,
                  "tcp connect loopback");

        int srv = accept(listen_fd, NULL, NULL);
        CHECK(srv >= 0, "tcp accept");

        /* Client recv side is empty — MSG_DONTWAIT must EAGAIN
         * immediately on the still-blocking fd. */
        check_recvmsg_eagain(cli, "tcp");

        close(srv);
        close(cli);
        close(listen_fd);
    }

    TEST_DONE();
}
