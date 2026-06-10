/*
 * test-unix-msg-peek
 *
 * Verifies that AF_UNIX SOCK_STREAM recvmsg(MSG_PEEK) does not consume
 * stream bytes and does not advance the cmsg byte-mark queue. A
 * subsequent non-PEEK recvmsg() must return the same bytes and deliver
 * the originally-pending cmsg.
 *
 * Without the fix the peek path advances the read index and pops the
 * front SCM_RIGHTS cmsg; the follow-up recvmsg observes empty data or
 * misses the ancillary attachment, breaking POSIX peek semantics.
 */

#include "test_framework.h"
#include <fcntl.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <unistd.h>

static ssize_t send_with_cmsg(int s, const char *buf, size_t len, int fd)
{
    struct iovec iov = { .iov_base = (void *)buf, .iov_len = len };
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
    memcpy(CMSG_DATA(cmh), &fd, sizeof(int));
    mh.msg_controllen = cmh->cmsg_len;
    return sendmsg(s, &mh, 0);
}

static ssize_t recvmsg_with_cmsg(int s, char *buf, size_t len,
                                 char *cbuf, size_t cbuf_len,
                                 struct msghdr *mh_out, int flags)
{
    struct iovec iov = { .iov_base = buf, .iov_len = len };
    memset(mh_out, 0, sizeof(*mh_out));
    mh_out->msg_iov = &iov;
    mh_out->msg_iovlen = 1;
    mh_out->msg_control = cbuf;
    mh_out->msg_controllen = cbuf_len;
    return recvmsg(s, mh_out, flags);
}

int main(void)
{
    TEST_START("AF_UNIX stream MSG_PEEK does not consume bytes or cmsg");

    int sv[2];
    CHECK_RET(socketpair(AF_UNIX, SOCK_STREAM, 0, sv), 0, "socketpair");

    int payload_fd = open("/dev/null", O_RDONLY);
    CHECK(payload_fd >= 0, "open /dev/null");

    /* Send 4 bytes "PEEK" with a SCM_RIGHTS cmsg attached. */
    CHECK_RET(send_with_cmsg(sv[0], "PEEK", 4, payload_fd), 4, "sendmsg PEEK+cmsg");

    /* First recv with MSG_PEEK: must return the data but must NOT
     * deliver the cmsg (delivering would consume the cmsg queue entry
     * and duplicate the SCM_RIGHTS fd). */
    char rxbuf[16];
    char rcbuf[CMSG_SPACE(sizeof(int))];
    struct msghdr mh1;
    ssize_t r1 = recvmsg_with_cmsg(sv[1], rxbuf, sizeof(rxbuf),
                                   rcbuf, sizeof(rcbuf), &mh1, MSG_PEEK);
    CHECK(r1 == 4, "recvmsg(MSG_PEEK) returns 4 bytes");
    CHECK(memcmp(rxbuf, "PEEK", 4) == 0, "MSG_PEEK delivers correct payload");
    struct cmsghdr *cmh1 = CMSG_FIRSTHDR(&mh1);
    CHECK(cmh1 == NULL, "MSG_PEEK does not deliver SCM_RIGHTS cmsg");

    /* Second recv WITHOUT MSG_PEEK: must observe the same bytes
     * (since peek did not advance the read index) AND deliver the
     * cmsg that peek left in place. */
    memset(rxbuf, 0, sizeof(rxbuf));
    char rcbuf2[CMSG_SPACE(sizeof(int))];
    struct msghdr mh2;
    ssize_t r2 = recvmsg_with_cmsg(sv[1], rxbuf, sizeof(rxbuf),
                                   rcbuf2, sizeof(rcbuf2), &mh2, 0);
    CHECK(r2 == 4, "follow-up recvmsg() returns the same 4 bytes");
    CHECK(memcmp(rxbuf, "PEEK", 4) == 0, "follow-up recvmsg delivers same payload");
    struct cmsghdr *cmh2 = CMSG_FIRSTHDR(&mh2);
    CHECK(cmh2 != NULL, "follow-up recvmsg delivers SCM_RIGHTS cmsg");
    if (cmh2 != NULL) {
        CHECK(cmh2->cmsg_level == SOL_SOCKET, "cmsg_level == SOL_SOCKET");
        CHECK(cmh2->cmsg_type == SCM_RIGHTS, "cmsg_type == SCM_RIGHTS");
        int rx_fd = -1;
        memcpy(&rx_fd, CMSG_DATA(cmh2), sizeof(int));
        CHECK(rx_fd >= 0, "received fd is valid");
        if (rx_fd >= 0) {
            close(rx_fd);
        }
    }

    /* A third recv with MSG_PEEK after the data has been consumed
     * must report empty (WouldBlock) rather than a stale read. */
    char tail[8];
    ssize_t r3 = recv(sv[1], tail, sizeof(tail), MSG_PEEK | MSG_DONTWAIT);
    CHECK(r3 == -1 && errno == EAGAIN,
          "MSG_PEEK|MSG_DONTWAIT after drain returns -1/EAGAIN");

    close(payload_fd);
    close(sv[0]);
    close(sv[1]);

    TEST_DONE();
}
