#define _POSIX_C_SOURCE 200809L

#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#define DNS_QUERY "apk.local A dl-cdn.alpinelinux.org"
#define DNS_REPLY "apk.local A 127.0.0.1"
#define INDEX_PATH "/alpine/APKINDEX.tar.gz"
#define PACKAGE_PATH "/alpine/main/x86_64/fake-package.apk"
#define INDEX_REQUEST_LINE "GET /alpine/APKINDEX.tar.gz HTTP/1.1"
#define PACKAGE_REQUEST_LINE "GET /alpine/main/x86_64/fake-package.apk HTTP/1.1"
#define INDEX_BODY "P:fake-package\nV:1.0-r0\nA:x86_64\n"
#define PACKAGE_BODY "fake apk payload\n"

static void fail_at(const char *file, int line, const char *message)
{
    printf("APK_NET_EQUIV_TEST_FAILED: %s:%d: %s (errno=%d %s)\n",
           file, line, message, errno, strerror(errno));
    fflush(stdout);
    exit(1);
}

#define CHECK(cond, message) \
    do { \
        if (!(cond)) { \
            fail_at(__FILE__, __LINE__, (message)); \
        } \
    } while (0)

static void write_all(int fd, const void *buf, size_t len)
{
    const char *p = (const char *)buf;
    while (len > 0) {
        ssize_t n = write(fd, p, len);
        CHECK(n > 0, "write failed");
        p += n;
        len -= (size_t)n;
    }
}

static void read_all(int fd, char *buf, size_t buf_len)
{
    size_t used = 0;
    while (used + 1 < buf_len) {
        ssize_t n = recv(fd, buf + used, buf_len - used - 1, 0);
        CHECK(n >= 0, "recv failed");
        if (n == 0) {
            break;
        }
        used += (size_t)n;
    }
    buf[used] = '\0';
}

static void set_socket_timeout(int fd)
{
    struct timeval tv = {
        .tv_sec = 5,
        .tv_usec = 0,
    };
    CHECK(setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv)) == 0,
          "setsockopt SO_RCVTIMEO failed");
    CHECK(setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv)) == 0,
          "setsockopt SO_SNDTIMEO failed");
}

static void wait_child(pid_t pid)
{
    int status = 0;
    CHECK(waitpid(pid, &status, 0) == pid, "waitpid failed");
    CHECK(WIFEXITED(status), "child did not exit normally");
    CHECK(WEXITSTATUS(status) == 0, "child reported failure");
}

static void test_dns_like_udp(void)
{
    int server = socket(AF_INET, SOCK_DGRAM, 0);
    int client = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(server >= 0 && client >= 0, "create UDP sockets failed");
    set_socket_timeout(server);
    set_socket_timeout(client);

    struct sockaddr_in server_addr = {
        .sin_family = AF_INET,
        .sin_port = 0,
        .sin_addr = { .s_addr = htonl(INADDR_LOOPBACK) },
    };
    CHECK(bind(server, (struct sockaddr *)&server_addr, sizeof(server_addr)) == 0,
          "bind UDP DNS server failed");

    socklen_t server_len = sizeof(server_addr);
    CHECK(getsockname(server, (struct sockaddr *)&server_addr, &server_len) == 0,
          "getsockname UDP DNS server failed");
    CHECK(ntohs(server_addr.sin_port) != 0, "UDP DNS server did not get a port");

    ssize_t sent = sendto(client, DNS_QUERY, sizeof(DNS_QUERY), 0,
                          (struct sockaddr *)&server_addr, sizeof(server_addr));
    CHECK(sent == (ssize_t)sizeof(DNS_QUERY), "sendto DNS query failed");

    char query[128] = {0};
    struct sockaddr_in peer = {0};
    socklen_t peer_len = sizeof(peer);
    ssize_t n = recvfrom(server, query, sizeof(query), 0,
                         (struct sockaddr *)&peer, &peer_len);
    CHECK(n == (ssize_t)sizeof(DNS_QUERY), "recvfrom DNS query size mismatch");
    CHECK(memcmp(query, DNS_QUERY, sizeof(DNS_QUERY)) == 0,
          "DNS query payload mismatch");
    CHECK(peer.sin_family == AF_INET, "DNS client peer family mismatch");
    CHECK(ntohl(peer.sin_addr.s_addr) == INADDR_LOOPBACK,
          "DNS client peer address mismatch");
    CHECK(ntohs(peer.sin_port) != 0, "DNS client peer port missing");

    sent = sendto(server, DNS_REPLY, sizeof(DNS_REPLY), 0,
                  (struct sockaddr *)&peer, peer_len);
    CHECK(sent == (ssize_t)sizeof(DNS_REPLY), "sendto DNS reply failed");

    char reply[128] = {0};
    n = recvfrom(client, reply, sizeof(reply), 0, NULL, NULL);
    CHECK(n == (ssize_t)sizeof(DNS_REPLY), "recvfrom DNS reply size mismatch");
    CHECK(memcmp(reply, DNS_REPLY, sizeof(DNS_REPLY)) == 0,
          "DNS reply payload mismatch");

    CHECK(close(client) == 0, "close UDP client failed");
    CHECK(close(server) == 0, "close UDP server failed");
}

static const char *body_for_path(const char *path)
{
    if (strcmp(path, INDEX_PATH) == 0) {
        return INDEX_BODY;
    }
    if (strcmp(path, PACKAGE_PATH) == 0) {
        return PACKAGE_BODY;
    }
    return NULL;
}

static void handle_http_client(int accepted)
{
    set_socket_timeout(accepted);

    char request[512] = {0};
    ssize_t n = recv(accepted, request, sizeof(request) - 1, 0);
    CHECK(n > 0, "recv HTTP request failed");
    request[n] = '\0';

    char path[128] = {0};
    CHECK(sscanf(request, "GET %127s HTTP/1.1", path) == 1,
          "HTTP request line parse failed");
    CHECK(strstr(request, INDEX_REQUEST_LINE) != NULL ||
              strstr(request, PACKAGE_REQUEST_LINE) != NULL,
          "HTTP request line mismatch");
    CHECK(strstr(request, "Host: apk.local") != NULL, "HTTP Host header missing");
    CHECK(strstr(request, "Connection: close") != NULL,
          "HTTP Connection header missing");

    const char *body = body_for_path(path);
    CHECK(body != NULL, "unexpected HTTP apk path");

    char response[512];
    int len = snprintf(response, sizeof(response),
                       "HTTP/1.1 200 OK\r\n"
                       "Content-Length: %zu\r\n"
                       "Connection: close\r\n"
                       "\r\n"
                       "%s",
                       strlen(body), body);
    CHECK(len > 0 && (size_t)len < sizeof(response), "HTTP response too long");
    write_all(accepted, response, (size_t)len);
}

static void http_server_main(int listen_fd)
{
    for (int i = 0; i < 2; i++) {
        int accepted = accept(listen_fd, NULL, NULL);
        CHECK(accepted >= 0, "accept HTTP client failed");
        handle_http_client(accepted);
        CHECK(close(accepted) == 0, "close HTTP client failed");
    }
    CHECK(close(listen_fd) == 0, "close HTTP listener failed");
    _exit(0);
}

static void fetch_http_path(const struct sockaddr_in *server_addr,
                            const char *path,
                            const char *expected_body)
{
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(fd >= 0, "create HTTP client socket failed");
    set_socket_timeout(fd);

    CHECK(connect(fd, (const struct sockaddr *)server_addr, sizeof(*server_addr)) == 0,
          "connect HTTP client failed");

    char request[256];
    int request_len = snprintf(request, sizeof(request),
                               "GET %s HTTP/1.1\r\n"
                               "Host: apk.local\r\n"
                               "Connection: close\r\n"
                               "\r\n",
                               path);
    CHECK(request_len > 0 && (size_t)request_len < sizeof(request),
          "HTTP request too long");
    ssize_t sent = send(fd, request, (size_t)request_len, 0);
    CHECK(sent == request_len, "send HTTP request failed");

    char response[1024];
    read_all(fd, response, sizeof(response));
    CHECK(strstr(response, "HTTP/1.1 200 OK") != NULL,
          "HTTP status line missing");
    CHECK(strstr(response, "Content-Length:") != NULL,
          "Content-Length: header missing");
    CHECK(strstr(response, expected_body) != NULL, "HTTP body mismatch");

    CHECK(close(fd) == 0, "close HTTP client failed");
}

static void test_apk_like_http_fetches(void)
{
    int listen_fd = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(listen_fd >= 0, "create HTTP listener socket failed");
    set_socket_timeout(listen_fd);

    int one = 1;
    CHECK(setsockopt(listen_fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one)) == 0,
          "setsockopt SO_REUSEADDR failed");

    struct sockaddr_in server_addr = {
        .sin_family = AF_INET,
        .sin_port = 0,
        .sin_addr = { .s_addr = htonl(INADDR_LOOPBACK) },
    };
    CHECK(bind(listen_fd, (struct sockaddr *)&server_addr, sizeof(server_addr)) == 0,
          "bind HTTP server failed");
    CHECK(listen(listen_fd, 2) == 0, "listen HTTP server failed");

    socklen_t server_len = sizeof(server_addr);
    CHECK(getsockname(listen_fd, (struct sockaddr *)&server_addr, &server_len) == 0,
          "getsockname HTTP server failed");
    CHECK(ntohs(server_addr.sin_port) != 0, "HTTP server did not get a port");

    pid_t server_pid = fork();
    CHECK(server_pid >= 0, "fork HTTP server failed");
    if (server_pid == 0) {
        http_server_main(listen_fd);
    }

    CHECK(close(listen_fd) == 0, "close parent HTTP listener failed");

    fetch_http_path(&server_addr, INDEX_PATH, INDEX_BODY);
    fetch_http_path(&server_addr, PACKAGE_PATH, PACKAGE_BODY);
    wait_child(server_pid);
}

int main(void)
{
    signal(SIGPIPE, SIG_IGN);
    test_dns_like_udp();
    test_apk_like_http_fetches();
    printf("APK_NET_EQUIV_TEST_PASSED\n");
    return 0;
}
