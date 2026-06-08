#include "test.h"

#include <arpa/inet.h>
#include <netdb.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#define HOST_HTTP_BODY "ArceOS C test suite host fixture\n"
#define HOST_HTTP_PORT "18080"
#define HTTP_OK_PREFIX "HTTP/1.1 200 OK"

static const char HTTP_REQUEST[] =
    "GET / HTTP/1.1\r\n"
    "Host: axbuild.local\r\n"
    "Accept: */*\r\n"
    "\r\n";

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
    puts("net_http: host HTTP APIs OK");
    return 0;
}
