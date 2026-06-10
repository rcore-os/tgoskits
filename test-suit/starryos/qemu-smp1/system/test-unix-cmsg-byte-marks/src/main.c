/*
 * test-unix-cmsg-byte-marks
 *
 * Verifies that on AF_UNIX SOCK_STREAM the cmsg attached to one sendmsg
 * is delivered with the recv that reaches that send's first byte, and
 * that a recv does not cross into a later cmsg-bearing message.
 *
 * Sequence:
 *   send #1: 4 bytes "AAAA" with cmsg-carrying fd
 *   send #2: 4 bytes "BBBB" no cmsg
 *   send #3: 4 bytes "CCCC" with cmsg-carrying fd
 *
 * recv #1 with a 16-byte buffer should:
 *   - return at most 8 bytes (4 from #1 + 4 from #2),
 *   - deliver the cmsg from send #1
 * recv #2 with a 16-byte buffer should:
 *   - return 4 bytes ("CCCC")
 *   - deliver the cmsg from send #3
 */

#include "test_framework.h"
#include <sys/socket.h>
#include <sys/uio.h>
#include <unistd.h>
#include <string.h>
#include <fcntl.h>

static ssize_t send_with_optional_cmsg(int s, const char *buf, size_t len, int fd)
{
    struct iovec iov = { .iov_base = (void *)buf, .iov_len = len };
    char cbuf[CMSG_SPACE(sizeof(int))];
    memset(cbuf, 0, sizeof(cbuf));
    struct msghdr mh = {0};
    mh.msg_iov = &iov;
    mh.msg_iovlen = 1;
    if (fd >= 0) {
        mh.msg_control = cbuf;
        mh.msg_controllen = sizeof(cbuf);
        struct cmsghdr *cmh = CMSG_FIRSTHDR(&mh);
        cmh->cmsg_level = SOL_SOCKET;
        cmh->cmsg_type = SCM_RIGHTS;
        cmh->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(cmh), &fd, sizeof(int));
        mh.msg_controllen = cmh->cmsg_len;
    }
    return sendmsg(s, &mh, 0);
}

int main(void)
{
    TEST_START("AF_UNIX cmsg byte-mark per-message boundary");

    int sv[2];
    CHECK_RET(socketpair(AF_UNIX, SOCK_STREAM, 0, sv), 0, "socketpair");

    int dummy = open("/dev/null", O_RDONLY);
    CHECK(dummy >= 0, "open /dev/null");

    CHECK_RET(send_with_optional_cmsg(sv[0], "AAAA", 4, dummy), 4, "send #1 + cmsg");
    CHECK_RET(send_with_optional_cmsg(sv[0], "BBBB", 4, -1),    4, "send #2 no cmsg");
    CHECK_RET(send_with_optional_cmsg(sv[0], "CCCC", 4, dummy), 4, "send #3 + cmsg");

    /* recv #1: large buffer; should return up to the boundary of send #3. */
    char rxbuf[16];
    char rcbuf[CMSG_SPACE(sizeof(int))];
    struct iovec riov = { .iov_base = rxbuf, .iov_len = sizeof(rxbuf) };
    struct msghdr rmh = {0};
    rmh.msg_iov = &riov;
    rmh.msg_iovlen = 1;
    rmh.msg_control = rcbuf;
    rmh.msg_controllen = sizeof(rcbuf);

    ssize_t r1 = recvmsg(sv[1], &rmh, 0);
    CHECK(r1 > 0 && r1 <= 8, "recv #1 returns 1..=8 bytes (does not cross into send #3)");
    CHECK(rxbuf[0] == 'A', "recv #1 first byte == 'A'");
    /* cmsg from send #1 must be delivered with this recv. */
    struct cmsghdr *cmh = CMSG_FIRSTHDR(&rmh);
    CHECK(cmh != NULL, "cmsg from send #1 delivered with recv #1");

    /* recv #2: should return at least the remaining 4 bytes ("CCCC"). */
    memset(rxbuf, 0, sizeof(rxbuf));
    memset(rcbuf, 0, sizeof(rcbuf));
    rmh.msg_control = rcbuf;
    rmh.msg_controllen = sizeof(rcbuf);
    ssize_t r2 = recvmsg(sv[1], &rmh, 0);
    CHECK(r2 > 0, "recv #2 returns more bytes");
    /* If recv #1 returned 8 bytes, recv #2 should be 4 bytes 'C'. */
    if (r1 == 8) {
        CHECK(r2 == 4 && rxbuf[0] == 'C', "recv #2 == 4 bytes 'C'");
        struct cmsghdr *cmh2 = CMSG_FIRSTHDR(&rmh);
        CHECK(cmh2 != NULL, "cmsg from send #3 delivered with recv #2");
    }

    close(dummy);
    close(sv[0]);
    close(sv[1]);

    /* Scenario 2: a recv that consumes the first byte of a cmsg-bearing
     * message but DOES NOT pass msg_control. The cmsg must still be
     * dropped; a later recvmsg with msg_control must not see it. */
    CHECK_RET(socketpair(AF_UNIX, SOCK_STREAM, 0, sv), 0, "socketpair (2)");
    int dummy2 = open("/dev/null", O_RDONLY);
    CHECK(dummy2 >= 0, "open /dev/null (2)");

    CHECK_RET(send_with_optional_cmsg(sv[0], "X", 1, dummy2), 1, "send 1 byte + cmsg");
    CHECK_RET(send_with_optional_cmsg(sv[0], "Y", 1, -1),     1, "send 1 byte no cmsg");

    /* recv with read(): no msg_control. Must still drop the pending cmsg. */
    char b;
    CHECK_RET(read(sv[1], &b, 1), 1, "plain read of first byte");
    CHECK(b == 'X', "plain read returned 'X'");

    /* Second recv with msg_control. It must NOT carry the dropped cmsg. */
    struct iovec riov2 = { .iov_base = &b, .iov_len = 1 };
    struct msghdr rmh2 = {0};
    char rcbuf2[CMSG_SPACE(sizeof(int))];
    memset(rcbuf2, 0, sizeof(rcbuf2));
    rmh2.msg_iov = &riov2;
    rmh2.msg_iovlen = 1;
    rmh2.msg_control = rcbuf2;
    rmh2.msg_controllen = sizeof(rcbuf2);
    CHECK_RET(recvmsg(sv[1], &rmh2, 0), 1, "second recvmsg");
    CHECK(b == 'Y', "second recvmsg returned 'Y'");
    CHECK(CMSG_FIRSTHDR(&rmh2) == NULL,
          "cmsg from send #1 was dropped (not delivered with later recvmsg)");

    close(dummy2);
    close(sv[0]);
    close(sv[1]);

    TEST_DONE();
}
