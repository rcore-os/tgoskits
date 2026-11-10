#include "test.h"

#include <arpa/inet.h>
#include <errno.h>
#include <netdb.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <unistd.h>

#define HOST_HTTP_BODY "ArceOS C test suite host fixture\n"

#if defined(__x86_64__)
#define HOST_HTTP_PORT "18280"
#elif defined(__aarch64__)
#define HOST_HTTP_PORT "18281"
#elif defined(__riscv) && __riscv_xlen == 64
#define HOST_HTTP_PORT "18282"
#elif defined(__loongarch64)
#define HOST_HTTP_PORT "18283"
#else
#define HOST_HTTP_PORT "18080"
#endif

#define HTTP_OK_PREFIX "HTTP/1.1 200 OK"

static const char HTTP_REQUEST[] =
    "GET / HTTP/1.1\r\n"
    "Host: axbuild.local\r\n"
    "Accept: */*\r\n"
    "\r\n";

/* Keep the edge-triggered watcher from sampling the empty accept queue.
 * A separate level-triggered watcher synchronizes each completed handshake. */
static int test_accept_edge_after_drain(char *reason, size_t reason_len)
{
    int listener = -1, epfd = -1, probe = -1;
    int clients[3] = {-1, -1, -1};
    int accepted[3] = {-1, -1, -1};
    int result = -1;
    const char *stage = "create listener";
    struct sockaddr_in addr = {0};
    socklen_t addr_len = sizeof(addr);
    struct epoll_event interest = {0}, ready = {0};

    listener = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (listener < 0)
        goto out;
    addr.sin_family = AF_INET;
    if (inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr) != 1)
        goto out;
    stage = "bind/listen";
    if (bind(listener, (struct sockaddr *)&addr, sizeof(addr)) != 0 ||
        listen(listener, 4) != 0 ||
        getsockname(listener, (struct sockaddr *)&addr, &addr_len) != 0)
        goto out;

    epfd = epoll_create1(0);
    probe = epoll_create1(0);
    stage = "register accept watchers";
    if (epfd < 0 || probe < 0)
        goto out;
    interest.events = EPOLLIN | EPOLLET;
    interest.data.fd = listener;
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, listener, &interest) != 0)
        goto out;
    interest.events = EPOLLIN;
    if (epoll_ctl(probe, EPOLL_CTL_ADD, listener, &interest) != 0)
        goto out;

    for (int round = 0; round < 3; round++) {
        struct sockaddr_in peer = {0};
        socklen_t peer_len = sizeof(peer);
        stage = "connect next client";
        clients[round] = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
        if (clients[round] < 0 ||
            connect(clients[round], (struct sockaddr *)&addr, sizeof(addr)) != 0)
            goto out;
        stage = "wait for completed handshake";
        if (epoll_wait(probe, &ready, 1, 5000) != 1 ||
            ready.data.fd != listener || !(ready.events & EPOLLIN))
            goto out;

        /* The preceding accept drained the queue, but epfd did not observe
         * that interval. Each new connection must still produce an edge. */
        if (epoll_wait(epfd, &ready, 1, 0) != 1 ||
            ready.data.fd != listener || !(ready.events & EPOLLIN)) {
            test_fail(reason, reason_len, "accept edge lost after drain: round=%d", round);
            goto cleanup;
        }
        stage = "accept the only pending client";
        accepted[round] = accept(listener, (struct sockaddr *)&peer, &peer_len);
        if (accepted[round] < 0)
            goto out;
    }
    result = 0;
    puts("net_http: accept edges survive unsampled empty queue");
    goto cleanup;
out:
    test_fail(reason, reason_len, "accept edge regression: %s errno=%d", stage, errno);
cleanup:
    for (int round = 0; round < 3; round++) {
        if (accepted[round] >= 0)
            close(accepted[round]);
        if (clients[round] >= 0)
            close(clients[round]);
    }
    if (probe >= 0)
        close(probe);
    if (epfd >= 0)
        close(epfd);
    if (listener >= 0)
        close(listener);
    return result;
}

int arceos_c_test_net_http(char *reason, size_t reason_len)
{
    struct addrinfo hints;
    struct addrinfo *res = NULL;
    char ip[INET_ADDRSTRLEN];
    int sock = -1;
    char response[512];
    ssize_t len;
    size_t total = 0;

    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;

    CHECK_RET(getaddrinfo("10.0.2.2", HOST_HTTP_PORT, &hints, &res), 0);
    CHECK_TRUE(res != NULL);
    CHECK_TRUE(inet_ntop(AF_INET, &((struct sockaddr_in *)res->ai_addr)->sin_addr, ip,
                         sizeof(ip)) != NULL);
    CHECK_RET(strcmp(ip, "10.0.2.2"), 0);

    sock = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    CHECK_TRUE(sock >= 0);
    if (connect(sock, res->ai_addr, res->ai_addrlen) != 0) {
        freeaddrinfo(res);
        close(sock);
        test_fail(reason, reason_len, "connect host HTTP fixture failed");
        return -1;
    }
    CHECK_RET(send(sock, HTTP_REQUEST, strlen(HTTP_REQUEST), 0), (ssize_t)strlen(HTTP_REQUEST));
    while (total < sizeof(response) - 1) {
        len = recv(sock, response + total, sizeof(response) - 1 - total, 0);
        if (len < 0) {
            freeaddrinfo(res);
            close(sock);
            test_fail(reason, reason_len, "recv host HTTP fixture failed");
            return -1;
        }
        if (len == 0) {
            break;
        }
        total += (size_t)len;
    }
    if (total == 0) {
        freeaddrinfo(res);
        close(sock);
        test_fail(reason, reason_len, "recv host HTTP fixture returned empty response");
        return -1;
    }
    response[total] = '\0';
    if (total < strlen(HTTP_OK_PREFIX) ||
        memcmp(response, HTTP_OK_PREFIX, strlen(HTTP_OK_PREFIX)) != 0) {
        freeaddrinfo(res);
        close(sock);
        test_fail(reason, reason_len, "missing HTTP 200 status in response: %s", response);
        return -1;
    }
    if (total < strlen(HOST_HTTP_BODY) ||
        memcmp(response + total - strlen(HOST_HTTP_BODY), HOST_HTTP_BODY,
               strlen(HOST_HTTP_BODY)) != 0) {
        freeaddrinfo(res);
        close(sock);
        test_fail(reason, reason_len, "missing fixture body in response: %s", response);
        return -1;
    }

    freeaddrinfo(res);
    close(sock);
    if (test_accept_edge_after_drain(reason, reason_len) != 0)
        return -1;
    puts("net_http: host HTTP APIs OK");
    return 0;
}
