// echod - a tiny static HTTP echo backend for the higress carpet.
//
// The Alpine base rootfs busybox is built without the `httpd` applet, so the
// gateway's upstream backends are provided by this self-contained server
// instead. It is cross-compiled to a static musl binary at prebuild time (no
// runtime dependency, runs directly on StarryOS), and forks one child per
// connection. The reply body echoes the received method, request URI and a few
// request headers so the carpet can assert routing, path/host rewriting and
// request-header mutation; a fixed `X-Backend-Secret` response header lets it
// assert response-header removal.
//
// usage: echod <port> <backend-id> <mode>    mode = ok | fail503 | slow
#include <arpa/inet.h>
#include <netinet/in.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

static void header_value(const char *line, const char *name, size_t nlen, char *out, size_t osize) {
    if (strncasecmp(line, name, nlen) != 0) {
        return;
    }
    const char *p = line + nlen;
    while (*p == ' ' || *p == '\t') {
        p++;
    }
    size_t i = 0;
    while (*p && *p != '\r' && *p != '\n' && i + 1 < osize) {
        out[i++] = *p++;
    }
    out[i] = '\0';
}

static void handle(int fd, const char *id, const char *mode) {
    char buf[16384];
    size_t total = 0;
    while (total + 1 < sizeof(buf)) {
        ssize_t n = read(fd, buf + total, sizeof(buf) - 1 - total);
        if (n <= 0) {
            break;
        }
        total += (size_t)n;
        buf[total] = '\0';
        if (strstr(buf, "\r\n\r\n") != NULL) {
            break;
        }
    }
    if (total == 0) {
        close(fd);
        return;
    }
    buf[total] = '\0';

    char method[16] = "";
    char uri[4096] = "";
    sscanf(buf, "%15s %4095s", method, uri);

    char host[256] = "", added[256] = "", canary[256] = "", strip[256] = "";
    char *save = NULL;
    char *line = strtok_r(buf, "\r\n", &save); // request line
    while ((line = strtok_r(NULL, "\r\n", &save)) != NULL) {
        header_value(line, "host:", 5, host, sizeof(host));
        header_value(line, "x-higress-added:", 16, added, sizeof(added));
        header_value(line, "x-canary:", 9, canary, sizeof(canary));
        header_value(line, "x-strip-me:", 11, strip, sizeof(strip));
    }

    if (strcmp(mode, "slow") == 0) {
        sleep(4);
    }
    int status = 200;
    const char *reason = "OK";
    if (strcmp(mode, "fail503") == 0) {
        status = 503;
        reason = "Service Unavailable";
    }

    char body[8192];
    int blen = snprintf(body, sizeof(body),
                        "BACKEND=%s\nREQ_URI=%s\nMETHOD=%s\nHOST=%s\n"
                        "X_HIGRESS_ADDED=%s\nX_CANARY=%s\nX_STRIP_ME=%s\n",
                        id, uri, method, host, added, canary, strip);

    char head[512];
    int hlen = snprintf(head, sizeof(head),
                        "HTTP/1.1 %d %s\r\nContent-Type: text/plain\r\n"
                        "Content-Length: %d\r\nX-Backend-Secret: leak\r\n"
                        "Connection: close\r\n\r\n",
                        status, reason, blen);
    if (write(fd, head, (size_t)hlen) == hlen && strcmp(method, "HEAD") != 0) {
        (void)!write(fd, body, (size_t)blen);
    }
    close(fd);
}

int main(int argc, char **argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: echod <port> <backend-id> <ok|fail503|slow>\n");
        return 2;
    }
    signal(SIGPIPE, SIG_IGN);

    int srv = socket(AF_INET, SOCK_STREAM, 0);
    if (srv < 0) {
        perror("socket");
        return 1;
    }
    int one = 1;
    setsockopt(srv, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr.sin_port = htons((unsigned short)atoi(argv[1]));
    if (bind(srv, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        perror("bind");
        return 1;
    }
    if (listen(srv, 64) < 0) {
        perror("listen");
        return 1;
    }
    printf("echod %s on 127.0.0.1:%s mode=%s\n", argv[2], argv[1], argv[3]);
    fflush(stdout);

    for (;;) {
        int cli = accept(srv, NULL, NULL);
        if (cli < 0) {
            continue;
        }
        pid_t pid = fork();
        if (pid == 0) {
            close(srv);
            handle(cli, argv[2], argv[3]);
            _exit(0);
        }
        close(cli);
        while (waitpid(-1, NULL, WNOHANG) > 0) {
            // reap finished children
        }
    }
}
