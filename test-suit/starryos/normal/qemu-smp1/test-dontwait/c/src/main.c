/*
 * test-dontwait
 *
 * Verifies that recvmsg() with MSG_DONTWAIT on an empty stream socket
 * returns EAGAIN immediately rather than blocking. The socket itself
 * is left blocking; only the per-call flag is the override.
 */

#include "test_framework.h"
#include <sys/socket.h>
#include <sys/uio.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>

#ifndef MSG_DONTWAIT
#define MSG_DONTWAIT 0x40
#endif

int main(void)
{
    TEST_START("MSG_DONTWAIT returns EAGAIN on empty stream socket");

    int sv[2];
    CHECK_RET(socketpair(AF_UNIX, SOCK_STREAM, 0, sv), 0, "socketpair");

    /* Confirm socket is blocking. */
    int fl = fcntl(sv[1], F_GETFL, 0);
    CHECK((fl & O_NONBLOCK) == 0, "socket is blocking");

    char buf[8];
    struct iovec iov = { .iov_base = buf, .iov_len = sizeof(buf) };
    struct msghdr mh = {0};
    mh.msg_iov = &iov;
    mh.msg_iovlen = 1;

    errno = 0;
    ssize_t r = recvmsg(sv[1], &mh, MSG_DONTWAIT);
    CHECK(r < 0, "recvmsg(MSG_DONTWAIT) on empty socket returns -1");
    CHECK(errno == EAGAIN || errno == EWOULDBLOCK,
          "errno is EAGAIN/EWOULDBLOCK");

    /* After the call, the socket must still be blocking (the flip should
     * be per-call, not a persistent state change). */
    int fl2 = fcntl(sv[1], F_GETFL, 0);
    CHECK((fl2 & O_NONBLOCK) == 0, "socket still blocking after MSG_DONTWAIT recv");

    close(sv[0]);
    close(sv[1]);

    TEST_DONE();
}
