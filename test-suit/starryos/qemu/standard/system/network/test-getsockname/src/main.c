#define _GNU_SOURCE
#include "test_framework.h"
#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void)
{
    TEST_START("getsockname");

    /* ================================================================
     * 1. TCP 绑定后 getsockname — 验证返回地址与绑定地址一致
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(fd >= 0, "socket(AF_INET, SOCK_STREAM) creates TCP socket");

        struct sockaddr_in bind_addr = {
            .sin_family = AF_INET,
            .sin_port = 0,
            .sin_addr = { .s_addr = htonl(INADDR_LOOPBACK) },
        };
        CHECK_RET(bind(fd, (struct sockaddr *)&bind_addr, sizeof(bind_addr)),
                  0, "bind to 127.0.0.1:0");

        struct sockaddr_in got = {0};
        socklen_t got_len = sizeof(got);
        CHECK_RET(getsockname(fd, (struct sockaddr *)&got, &got_len), 0,
                  "getsockname on bound TCP socket returns 0");
        CHECK(got.sin_family == AF_INET,
              "getsockname reports AF_INET");
        CHECK(got.sin_port != 0,
              "getsockname reports non-zero port");
        CHECK(got.sin_addr.s_addr == htonl(INADDR_LOOPBACK),
              "getsockname reports loopback address");
        CHECK(got_len == sizeof(struct sockaddr_in),
              "getsockname addrlen equals sizeof(sockaddr_in)");

        close(fd);
    }

    /* ================================================================
     * 2. UDP 绑定后 getsockname — 验证地址族和端口
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_DGRAM, 0);
        CHECK(fd >= 0, "socket(AF_INET, SOCK_DGRAM) creates UDP socket");

        struct sockaddr_in bind_addr = {
            .sin_family = AF_INET,
            .sin_port = 0,
            .sin_addr = { .s_addr = htonl(INADDR_ANY) },
        };
        CHECK_RET(bind(fd, (struct sockaddr *)&bind_addr, sizeof(bind_addr)),
                  0, "bind UDP to INADDR_ANY:0");

        struct sockaddr_in got = {0};
        socklen_t got_len = sizeof(got);
        CHECK_RET(getsockname(fd, (struct sockaddr *)&got, &got_len), 0,
                  "getsockname on bound UDP socket returns 0");
        CHECK(got.sin_family == AF_INET,
              "UDP getsockname reports AF_INET");
        CHECK(got.sin_port != 0,
              "UDP getsockname reports non-zero port");

        close(fd);
    }

    /* ================================================================
     * 3. 未绑定 socket — getsockname 应成功，端口为 0
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(fd >= 0, "create unbound TCP socket");

        struct sockaddr_in got = {0};
        socklen_t got_len = sizeof(got);
        CHECK_RET(getsockname(fd, (struct sockaddr *)&got, &got_len), 0,
                  "getsockname on unbound socket returns success");
        CHECK(got.sin_family == AF_INET,
              "unbound getsockname reports AF_INET");
        CHECK(got.sin_port == 0,
              "unbound getsockname reports port=0");

        close(fd);
    }

    /* ================================================================
     * 4. 无效 fd (-1) — 应返回 EBADF
     * ================================================================ */
    {
        struct sockaddr_in got = {0};
        socklen_t got_len = sizeof(got);
        CHECK_ERR(getsockname(-1, (struct sockaddr *)&got, &got_len),
                  EBADF, "getsockname(fd=-1) returns EBADF");
    }

    /* ================================================================
     * 5. 已关闭的 fd — 应返回 EBADF
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(fd >= 0, "create socket for close test");
        close(fd);

        struct sockaddr_in got = {0};
        socklen_t got_len = sizeof(got);
        CHECK_ERR(getsockname(fd, (struct sockaddr *)&got, &got_len),
                  EBADF, "getsockname on closed fd returns EBADF");
    }

    /* ================================================================
     * 6. 非 socket fd — 应返回 ENOTSOCK
     * ================================================================ */
    {
        int fd = open("/dev/null", O_RDONLY);
        CHECK(fd >= 0, "open /dev/null for non-socket test");

        struct sockaddr_in got = {0};
        socklen_t got_len = sizeof(got);
        CHECK_ERR(getsockname(fd, (struct sockaddr *)&got, &got_len),
                  ENOTSOCK, "getsockname on non-socket fd returns ENOTSOCK");

        close(fd);
    }

    /* ================================================================
     * 7. addrlen=0 — getsockname 应成功并将 addrlen 更新为实际大小
     *    Linux 行为: 返回 0，addrlen 被设为实际地址长度
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(fd >= 0, "create socket for addrlen=0 test");

        struct sockaddr_in bind_addr = {
            .sin_family = AF_INET,
            .sin_port = 0,
            .sin_addr = { .s_addr = htonl(INADDR_LOOPBACK) },
        };
        CHECK_RET(bind(fd, (struct sockaddr *)&bind_addr, sizeof(bind_addr)),
                  0, "bind for addrlen=0 test");

        char buf[256] = {0};
        socklen_t len = 0;
        int ret = getsockname(fd, (struct sockaddr *)buf, &len);
        CHECK(ret == 0, "getsockname with addrlen=0 returns 0");
        CHECK(len > 0, "getsockname with addrlen=0 updates addrlen > 0");
        CHECK(len == sizeof(struct sockaddr_in),
              "getsockname with addrlen=0 reports sizeof(sockaddr_in)");

        close(fd);
    }

    TEST_DONE();
}
