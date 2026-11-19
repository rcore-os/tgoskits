#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <poll.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void)
{
    TEST_START("shutdown");

    /* ================================================================
     * 1. shutdown connected socket with SHUT_WR
     *
     * Verify that SHUT_WR makes the peer pollable and returns EOF from
     * recv. Nix sandbox builders rely on this control-socket transition;
     * missing peer EOF leaves the builder and its supervisor blocked.
     * ================================================================ */
    {
        int sv[2];
        CHECK_RET(socketpair(AF_UNIX, SOCK_STREAM, 0, sv), 0,
                  "create socketpair");
        CHECK_RET(shutdown(sv[0], SHUT_WR), 0,
                  "shutdown SHUT_WR returns 0");

        struct pollfd peer = {
            .fd = sv[1],
            .events = POLLIN | POLLRDHUP,
        };
        CHECK_RET(poll(&peer, 1, 1000), 1,
                  "peer observes SHUT_WR readiness");
        CHECK((peer.revents & (POLLIN | POLLRDHUP)) != 0,
              "peer reports readable EOF or read hangup");

        CHECK_RET(fcntl(sv[1], F_SETFL, O_NONBLOCK), 0,
                  "set peer nonblocking before EOF recv");
        char byte;
        CHECK_RET((int)recv(sv[1], &byte, sizeof(byte), 0), 0,
                  "peer recv returns EOF after SHUT_WR");
        close(sv[0]);
        close(sv[1]);
    }

    /* ================================================================
     * 2. shutdown connected socket with SHUT_RD
     * ================================================================ */
    {
        int sv[2];
        CHECK_RET(socketpair(AF_UNIX, SOCK_STREAM, 0, sv), 0,
                  "create socketpair");
        CHECK_RET(shutdown(sv[0], SHUT_RD), 0,
                  "shutdown SHUT_RD returns 0");
        close(sv[0]);
        close(sv[1]);
    }

    /* ================================================================
     * 3. shutdown connected socket with SHUT_RDWR
     * ================================================================ */
    {
        int sv[2];
        CHECK_RET(socketpair(AF_UNIX, SOCK_STREAM, 0, sv), 0,
                  "create socketpair");
        CHECK_RET(shutdown(sv[0], SHUT_RDWR), 0,
                  "shutdown SHUT_RDWR returns 0");
        close(sv[0]);
        close(sv[1]);
    }

    /* ================================================================
     * 4. 无效 how (99) — EINVAL
     * ================================================================ */
    {
        int sv[2];
        CHECK_RET(socketpair(AF_UNIX, SOCK_STREAM, 0, sv), 0,
                  "create socketpair");
        CHECK_ERR(shutdown(sv[0], 99),
                  EINVAL, "shutdown with invalid how returns EINVAL");
        close(sv[0]);
        close(sv[1]);
    }

    /* ================================================================
     * 5. 无效 fd (-1) — EBADF
     * ================================================================ */
    {
        CHECK_ERR(shutdown(-1, SHUT_RDWR),
                  EBADF, "shutdown(fd=-1) returns EBADF");
    }

    /* ================================================================
     * 6. 已关闭 fd — EBADF
     * ================================================================ */
    {
        int sv[2];
        CHECK_RET(socketpair(AF_UNIX, SOCK_STREAM, 0, sv), 0,
                  "create socketpair");
        close(sv[0]);

        CHECK_ERR(shutdown(sv[0], SHUT_RDWR),
                  EBADF, "shutdown on closed fd returns EBADF");

        close(sv[1]);
    }

    /* ================================================================
     * 7. 非 socket fd — ENOTSOCK
     * ================================================================ */
    {
        int fd = open("/dev/null", O_RDONLY);
        CHECK(fd >= 0, "open /dev/null for non-socket test");

        CHECK_ERR(shutdown(fd, SHUT_RDWR),
                  ENOTSOCK, "shutdown on non-socket fd returns ENOTSOCK");

        close(fd);
    }

    TEST_DONE();
}
