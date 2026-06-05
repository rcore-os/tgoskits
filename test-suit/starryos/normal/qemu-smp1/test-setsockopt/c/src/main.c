#define _GNU_SOURCE
#include "test_framework.h"
#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void)
{
    TEST_START("setsockopt");

    /* ================================================================
     * 1. SO_REUSEADDR — set then verify via getsockopt
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(fd >= 0, "create TCP socket");

        int flag = 1;
        CHECK_RET(setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &flag, sizeof(flag)), 0,
                  "setsockopt SO_REUSEADDR=1");

        int verify = 0;
        socklen_t len = sizeof(verify);
        CHECK_RET(getsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &verify, &len), 0,
                  "getsockopt SO_REUSEADDR");
        CHECK(verify == 1,
              "SO_REUSEADDR is 1");

        close(fd);
    }

    /* ================================================================
     * 2. SO_KEEPALIVE — set then verify
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(fd >= 0, "create TCP socket");

        int flag = 1;
        CHECK_RET(setsockopt(fd, SOL_SOCKET, SO_KEEPALIVE, &flag, sizeof(flag)), 0,
                  "setsockopt SO_KEEPALIVE=1");

        int verify = 0;
        socklen_t len = sizeof(verify);
        CHECK_RET(getsockopt(fd, SOL_SOCKET, SO_KEEPALIVE, &verify, &len), 0,
                  "getsockopt SO_KEEPALIVE");
        CHECK(verify == 1,
              "SO_KEEPALIVE is 1");

        close(fd);
    }

    /* ================================================================
     * 3. TCP_NODELAY — set then verify
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(fd >= 0, "create TCP socket");

        int flag = 1;
        CHECK_RET(setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &flag, sizeof(flag)), 0,
                  "setsockopt TCP_NODELAY=1");

        int verify = 0;
        socklen_t len = sizeof(verify);
        CHECK_RET(getsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &verify, &len), 0,
                  "getsockopt TCP_NODELAY");
        CHECK(verify == 1,
              "TCP_NODELAY is 1");

        close(fd);
    }

    /* ================================================================
     * 4. 无效 optname — ENOPROTOOPT
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(fd >= 0, "create socket for invalid optname test");

        int val = 1;
        CHECK_ERR(setsockopt(fd, SOL_SOCKET, 999, &val, sizeof(val)),
                  ENOPROTOOPT, "setsockopt with invalid optname returns ENOPROTOOPT");

        close(fd);
    }

    /* ================================================================
     * 5. 无效 fd (-1) — EBADF
     * ================================================================ */
    {
        int val = 1;
        CHECK_ERR(setsockopt(-1, SOL_SOCKET, SO_REUSEADDR, &val, sizeof(val)),
                  EBADF, "setsockopt(fd=-1) returns EBADF");
    }

    /* ================================================================
     * 6. 已关闭 fd — EBADF
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(fd >= 0, "create socket for close test");
        close(fd);

        int val = 1;
        CHECK_ERR(setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &val, sizeof(val)),
                  EBADF, "setsockopt on closed fd returns EBADF");
    }

    /* ================================================================
     * 7. 非 socket fd — ENOTSOCK
     * ================================================================ */
    {
        int fd = open("/dev/null", O_RDONLY);
        CHECK(fd >= 0, "open /dev/null for non-socket test");

        int val = 1;
        CHECK_ERR(setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &val, sizeof(val)),
                  ENOTSOCK, "setsockopt on non-socket fd returns ENOTSOCK");

        close(fd);
    }

    TEST_DONE();
}
