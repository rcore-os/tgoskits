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
    TEST_START("getsockopt");

    /* ================================================================
     * 1. SO_TYPE on TCP socket — 返回 SOCK_STREAM
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(fd >= 0, "create TCP socket");

        int so_type = 0;
        socklen_t len = sizeof(so_type);
        CHECK_RET(getsockopt(fd, SOL_SOCKET, SO_TYPE, &so_type, &len), 0,
                  "getsockopt SO_TYPE returns 0");
        CHECK(so_type == SOCK_STREAM,
              "getsockopt SO_TYPE reports SOCK_STREAM");
        CHECK(len == sizeof(int),
              "getsockopt optlen equals sizeof(int)");

        close(fd);
    }

    /* ================================================================
     * 2. SO_TYPE on UDP socket — 返回 SOCK_DGRAM
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_DGRAM, 0);
        CHECK(fd >= 0, "create UDP socket");

        int so_type = 0;
        socklen_t len = sizeof(so_type);
        CHECK_RET(getsockopt(fd, SOL_SOCKET, SO_TYPE, &so_type, &len), 0,
                  "getsockopt SO_TYPE on UDP returns 0");
        CHECK(so_type == SOCK_DGRAM,
              "getsockopt SO_TYPE reports SOCK_DGRAM");

        close(fd);
    }

    /* ================================================================
     * 3. SO_ERROR — 无错误时应返回 0
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(fd >= 0, "create socket for SO_ERROR test");

        int so_error = -1;
        socklen_t len = sizeof(so_error);
        CHECK_RET(getsockopt(fd, SOL_SOCKET, SO_ERROR, &so_error, &len), 0,
                  "getsockopt SO_ERROR returns 0");
        CHECK(so_error == 0,
              "getsockopt SO_ERROR reports no error");

        close(fd);
    }

    /* ================================================================
     * 4. TCP_NODELAY — TCP 层选项
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(fd >= 0, "create TCP socket for TCP_NODELAY test");

        int flag = -1;
        socklen_t len = sizeof(flag);
        int ret = getsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &flag, &len);
        CHECK(ret == 0,
              "getsockopt TCP_NODELAY returns success");
        CHECK(len == sizeof(int),
              "getsockopt TCP_NODELAY optlen correct");

        close(fd);
    }

    /* ================================================================
     * 5. 无效 optname — ENOPROTOOPT
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(fd >= 0, "create socket for invalid optname test");

        int val = 0;
        socklen_t len = sizeof(val);
        CHECK_ERR(getsockopt(fd, SOL_SOCKET, 999, &val, &len),
                  ENOPROTOOPT, "getsockopt with invalid optname returns ENOPROTOOPT");

        close(fd);
    }

    /* ================================================================
     * 6. 无效 fd (-1) — EBADF
     * ================================================================ */
    {
        int val = 0;
        socklen_t len = sizeof(val);
        CHECK_ERR(getsockopt(-1, SOL_SOCKET, SO_TYPE, &val, &len),
                  EBADF, "getsockopt(fd=-1) returns EBADF");
    }

    /* ================================================================
     * 7. 已关闭 fd — EBADF
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(fd >= 0, "create socket for close test");
        close(fd);

        int val = 0;
        socklen_t len = sizeof(val);
        CHECK_ERR(getsockopt(fd, SOL_SOCKET, SO_TYPE, &val, &len),
                  EBADF, "getsockopt on closed fd returns EBADF");
    }

    /* ================================================================
     * 8. 非 socket fd — ENOTSOCK
     * ================================================================ */
    {
        int fd = open("/dev/null", O_RDONLY);
        CHECK(fd >= 0, "open /dev/null for non-socket test");

        int val = 0;
        socklen_t len = sizeof(val);
        CHECK_ERR(getsockopt(fd, SOL_SOCKET, SO_TYPE, &val, &len),
                  ENOTSOCK, "getsockopt on non-socket fd returns ENOTSOCK");

        close(fd);
    }

    /* ================================================================
     * 9. optlen=0 — getsockopt 应成功并更新 optlen
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(fd >= 0, "create socket for optlen=0 test");

        int val = 0;
        socklen_t len = 0;
        int ret = getsockopt(fd, SOL_SOCKET, SO_TYPE, &val, &len);
        CHECK(ret == 0, "getsockopt with optlen=0 returns 0");

        close(fd);
    }

    TEST_DONE();
}
