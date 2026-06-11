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

int main(void)
{
    TEST_START("AF_UNIX SOCK_STREAM SCM_RIGHTS round-trip");

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
    mh.msg_controllen = cmh->cmsg_len;

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
        char m = 0;
        ssize_t rr = read(got_fd, &m, 1);
        CHECK_RET(rr, 1, "read 1 byte from received pipe fd");
        CHECK(m == marker, "received pipe carries the same marker byte");
        close(got_fd);
    }

    close(p[0]);
    close(p[1]);
    close(sv[0]);
    close(sv[1]);

    TEST_DONE();
}
