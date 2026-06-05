#define _GNU_SOURCE
#include "test_framework.h"
#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void)
{
    TEST_START("getpeername");

    /* ================================================================
     * 1. TCP 连接后 getpeername — 获取对端地址
     * ================================================================ */
    {
        int server = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(server >= 0, "create server socket");

        struct sockaddr_in srv_addr = {
            .sin_family = AF_INET,
            .sin_port = 0,
            .sin_addr = { .s_addr = htonl(INADDR_LOOPBACK) },
        };
        CHECK_RET(bind(server, (struct sockaddr *)&srv_addr, sizeof(srv_addr)),
                  0, "server bind to 127.0.0.1:0");

        socklen_t addr_len = sizeof(srv_addr);
        CHECK_RET(getsockname(server, (struct sockaddr *)&srv_addr, &addr_len),
                  0, "getsockname server to get port");

        CHECK_RET(listen(server, 1), 0, "listen on server socket");

        int client = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(client >= 0, "create client socket");

        CHECK_RET(connect(client, (struct sockaddr *)&srv_addr, sizeof(srv_addr)),
                  0, "connect to server");

        struct sockaddr_in peer = {0};
        socklen_t peer_len = sizeof(peer);
        CHECK_RET(getpeername(client, (struct sockaddr *)&peer, &peer_len),
                  0, "getpeername on connected TCP client returns 0");
        CHECK(peer.sin_family == AF_INET,
              "getpeername reports AF_INET");
        CHECK(peer.sin_port == srv_addr.sin_port,
              "getpeername reports correct peer port");
        CHECK(peer.sin_addr.s_addr == htonl(INADDR_LOOPBACK),
              "getpeername reports loopback address");
        CHECK(peer_len == sizeof(struct sockaddr_in),
              "getpeername addrlen equals sizeof(sockaddr_in)");

        close(client);
        close(server);
    }

    /* ================================================================
     * 2. UDP connect 后 getpeername — 获取对端地址
     * ================================================================ */
    {
        int server = socket(AF_INET, SOCK_DGRAM, 0);
        CHECK(server >= 0, "create UDP server socket");

        struct sockaddr_in srv_addr = {
            .sin_family = AF_INET,
            .sin_port = 0,
            .sin_addr = { .s_addr = htonl(INADDR_LOOPBACK) },
        };
        CHECK_RET(bind(server, (struct sockaddr *)&srv_addr, sizeof(srv_addr)),
                  0, "UDP server bind");

        socklen_t addr_len = sizeof(srv_addr);
        CHECK_RET(getsockname(server, (struct sockaddr *)&srv_addr, &addr_len),
                  0, "getsockname UDP server");

        int client = socket(AF_INET, SOCK_DGRAM, 0);
        CHECK(client >= 0, "create UDP client");

        CHECK_RET(connect(client, (struct sockaddr *)&srv_addr, sizeof(srv_addr)),
                  0, "UDP connect to server");

        struct sockaddr_in peer = {0};
        socklen_t peer_len = sizeof(peer);
        CHECK_RET(getpeername(client, (struct sockaddr *)&peer, &peer_len),
                  0, "getpeername on connected UDP client returns 0");
        CHECK(peer.sin_family == AF_INET,
              "UDP getpeername reports AF_INET");
        CHECK(peer.sin_port == srv_addr.sin_port,
              "UDP getpeername reports peer port");

        close(client);
        close(server);
    }

    /* ================================================================
     * 3. 未连接 socket — 应返回 ENOTCONN
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(fd >= 0, "create unconnected TCP socket");

        struct sockaddr_in peer = {0};
        socklen_t peer_len = sizeof(peer);
        CHECK_ERR(getpeername(fd, (struct sockaddr *)&peer, &peer_len),
                  ENOTCONN, "getpeername on unconnected socket returns ENOTCONN");

        close(fd);
    }

    /* ================================================================
     * 4. 无效 fd (-1) — 应返回 EBADF
     * ================================================================ */
    {
        struct sockaddr_in peer = {0};
        socklen_t peer_len = sizeof(peer);
        CHECK_ERR(getpeername(-1, (struct sockaddr *)&peer, &peer_len),
                  EBADF, "getpeername(fd=-1) returns EBADF");
    }

    /* ================================================================
     * 5. 已关闭 fd — 应返回 EBADF
     * ================================================================ */
    {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(fd >= 0, "create socket for close test");
        close(fd);

        struct sockaddr_in peer = {0};
        socklen_t peer_len = sizeof(peer);
        CHECK_ERR(getpeername(fd, (struct sockaddr *)&peer, &peer_len),
                  EBADF, "getpeername on closed fd returns EBADF");
    }

    /* ================================================================
     * 6. 非 socket fd — 应返回 ENOTSOCK
     * ================================================================ */
    {
        int fd = open("/dev/null", O_RDONLY);
        CHECK(fd >= 0, "open /dev/null for non-socket test");

        struct sockaddr_in peer = {0};
        socklen_t peer_len = sizeof(peer);
        CHECK_ERR(getpeername(fd, (struct sockaddr *)&peer, &peer_len),
                  ENOTSOCK, "getpeername on non-socket fd returns ENOTSOCK");

        close(fd);
    }

    /* ================================================================
     * 7. addrlen=0 — getpeername 应返回成功，addrlen 更新
     * ================================================================ */
    {
        int server = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(server >= 0, "create server for addrlen=0 test");

        struct sockaddr_in srv_addr = {
            .sin_family = AF_INET,
            .sin_port = 0,
            .sin_addr = { .s_addr = htonl(INADDR_LOOPBACK) },
        };
        CHECK_RET(bind(server, (struct sockaddr *)&srv_addr, sizeof(srv_addr)),
                  0, "server bind for addrlen=0 test");

        socklen_t addr_len = sizeof(srv_addr);
        CHECK_RET(getsockname(server, (struct sockaddr *)&srv_addr, &addr_len),
                  0, "getsockname for addrlen=0 test");

        CHECK_RET(listen(server, 1), 0, "listen for addrlen=0 test");

        int client = socket(AF_INET, SOCK_STREAM, 0);
        CHECK(client >= 0, "client for addrlen=0 test");
        CHECK_RET(connect(client, (struct sockaddr *)&srv_addr, sizeof(srv_addr)),
                  0, "connect for addrlen=0 test");

        char buf[256] = {0};
        socklen_t len = 0;
        int ret = getpeername(client, (struct sockaddr *)buf, &len);
        CHECK(ret == 0, "getpeername with addrlen=0 returns 0");
        CHECK(len > 0, "getpeername with addrlen=0 updates addrlen > 0");
        CHECK(len == sizeof(struct sockaddr_in),
              "getpeername with addrlen=0 reports sizeof(sockaddr_in)");

        close(client);
        close(server);
    }

    TEST_DONE();
}
