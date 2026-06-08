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
    len = recv(sock, response, sizeof(response) - 1, 0);
    if (len <= 0) {
        freeaddrinfo(res);
        close(sock);
        test_fail(reason, reason_len, "recv host HTTP fixture returned %ld", (long)len);
        return -1;
    }
    response[len] = '\0';
    CHECK_TRUE(strstr(response, "HTTP/1.1 200 OK") != NULL);
    CHECK_TRUE(strstr(response, HOST_HTTP_BODY) != NULL);

    freeaddrinfo(res);
    close(sock);
    puts("net_http: host HTTP APIs OK");
    return 0;
}
