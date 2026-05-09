/*
 * test-unix-stream-eof
 *
 * Exercises the EOF path for AF_UNIX SOCK_STREAM recv when the sender closes.
 *
 * Bug: StreamTransport::recv returned WouldBlock even after the remote write
 * end was dropped and the ring buffer was empty. The caller treated WouldBlock
 * as "more data may arrive" and parked forever on the socket.
 *
 * Fix: After reading 0 bytes from the ring buffer, check write_is_held(). If
 * the sender has dropped, return Ok(0) (EOF) instead of WouldBlock.
 *
 * Test:
 *   socketpair(AF_UNIX, SOCK_STREAM) gives sv[0] (reader) and sv[1] (sender).
 *   1. Write 5 bytes through sv[1].
 *   2. Close sv[1] -- sender side dropped.
 *   3. recv on sv[0] must return 5 (the buffered data).
 *   4. Set sv[0] non-blocking. recv again must return 0 (EOF), not -1/EAGAIN.
 *      If the bug is present the second recv returns EAGAIN because
 *      WouldBlock is returned before checking write_is_held.
 */

#include "test_framework.h"
#include <fcntl.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

static const char MSG[] = "hello";
#define MSG_LEN ((int)(sizeof(MSG) - 1))

int main(void)
{
    TEST_START("unix stream recv returns 0 (EOF) after sender close");

    int sv[2];
    CHECK_RET(socketpair(AF_UNIX, SOCK_STREAM, 0, sv), 0, "socketpair");

    CHECK_RET((int)write(sv[1], MSG, MSG_LEN), MSG_LEN,
              "write to sender side");
    close(sv[1]);

    char buf[64] = {0};
    int n1 = (int)recv(sv[0], buf, sizeof(buf), 0);
    CHECK(n1 == MSG_LEN, "first recv returns buffered bytes");

    int flags = fcntl(sv[0], F_GETFL, 0);
    CHECK(flags >= 0, "fcntl F_GETFL");
    CHECK_RET(fcntl(sv[0], F_SETFL, flags | O_NONBLOCK), 0, "set O_NONBLOCK");

    int n2 = (int)recv(sv[0], buf, sizeof(buf), 0);
    CHECK(n2 == 0, "second recv returns 0 (EOF) -- not -1/EAGAIN");

    close(sv[0]);

    TEST_DONE();
}
