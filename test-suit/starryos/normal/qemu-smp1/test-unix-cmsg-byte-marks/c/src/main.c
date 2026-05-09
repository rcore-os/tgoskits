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

    TEST_DONE();
}
