#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#define CLIENTS 8
#define IO_TIMEOUT_MS 30000
#define TEST_TIMEOUT_SEC 180

#define CHECK(cond, fmt, ...)                                                    \
    do {                                                                         \
        if (!(cond)) {                                                           \
            fprintf(stderr, "FAIL: " fmt " (errno=%d %s)\n", ##__VA_ARGS__,    \
                    errno, strerror(errno));                                     \
            return 1;                                                            \
        }                                                                        \
        printf("PASS: " fmt "\n", ##__VA_ARGS__);                              \
    } while (0)

static void on_alarm(int sig) {
    (void)sig;
    fprintf(stderr, "FAIL: test timed out\n");
    _exit(124);
}

static void loopback_addr(struct sockaddr_in *addr, unsigned short port) {
    memset(addr, 0, sizeof(*addr));
    addr->sin_family = AF_INET;
    addr->sin_port = htons(port);
    addr->sin_addr.s_addr = htonl(INADDR_LOOPBACK);
}

static int make_listener(unsigned short *port) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;

    int one = 1;
    (void)setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));

    struct sockaddr_in addr;
    loopback_addr(&addr, 0);

    if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        close(fd);
        return -1;
    }
    if (listen(fd, CLIENTS) < 0) {
        close(fd);
        return -1;
    }

    socklen_t len = sizeof(addr);
    if (getsockname(fd, (struct sockaddr *)&addr, &len) < 0) {
        close(fd);
        return -1;
    }
    *port = ntohs(addr.sin_port);
    return fd;
}

static void client_process(int idx, unsigned short port) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) _exit(10);

    struct sockaddr_in addr;
    loopback_addr(&addr, port);

    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) _exit(11);

    char msg[32];
    int len = snprintf(msg, sizeof(msg), "client-%d", idx);
    if (len <= 0 || write(fd, msg, (size_t)len) != len) _exit(12);

    close(fd);
    _exit(0);
}

static int wait_readable(int fd, int slot) {
    struct pollfd pfd;
    memset(&pfd, 0, sizeof(pfd));
    pfd.fd = fd;
    pfd.events = POLLIN;

    errno = 0;
    int ret = poll(&pfd, 1, IO_TIMEOUT_MS);
    if (ret == 1 && (pfd.revents & (POLLIN | POLLERR | POLLHUP)) != 0) {
        printf("PASS: payload ready on accepted client %d\n", slot);
        return 0;
    }

    fprintf(stderr,
            "FAIL: payload not ready on accepted client %d "
            "(poll ret=%d revents=0x%x errno=%d %s)\n",
            slot, ret, pfd.revents, errno, strerror(errno));
    return -1;
}

static int read_payload(int fd, int slot, int *idx_out) {
    if (wait_readable(fd, slot) != 0) return -1;

    char buf[64];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    if (n <= 0) {
        fprintf(stderr,
                "FAIL: read payload from accepted client %d "
                "(ret=%zd errno=%d %s)\n",
                slot, n, errno, strerror(errno));
        return -1;
    }
    buf[n] = '\0';

    int idx = -1;
    if (sscanf(buf, "client-%d", &idx) != 1) {
        fprintf(stderr, "FAIL: parse payload '%s'\n", buf);
        return -1;
    }

    printf("PASS: read payload '%s' from accepted client %d\n", buf, slot);
    *idx_out = idx;
    return 0;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    signal(SIGALRM, on_alarm);
    alarm(TEST_TIMEOUT_SEC);

    printf("=== bug-tcp-concurrent-connect ===\n");

    unsigned short port = 0;
    int listener = make_listener(&port);
    CHECK(listener >= 0, "listen on 127.0.0.1:%u", (unsigned)port);

    pid_t pids[CLIENTS];
    for (int i = 0; i < CLIENTS; i++) {
        pid_t pid = fork();
        if (pid == 0) {
            client_process(i, port);
        }
        CHECK(pid > 0, "fork client %d", i);
        pids[i] = pid;
    }

    int fds[CLIENTS];
    for (int i = 0; i < CLIENTS; i++) {
        fds[i] = accept(listener, NULL, NULL);
        CHECK(fds[i] >= 0, "accept client %d", i);
    }
    close(listener);

    int seen[CLIENTS] = {0};
    for (int i = 0; i < CLIENTS; i++) {
        int idx = -1;
        CHECK(read_payload(fds[i], i, &idx) == 0,
              "payload from accepted client %d is readable", i);
        close(fds[i]);
        CHECK(idx >= 0 && idx < CLIENTS, "payload client id in range");
        CHECK(!seen[idx], "payload client id %d is unique", idx);
        seen[idx] = 1;
    }

    for (int i = 0; i < CLIENTS; i++) {
        int status = 0;
        CHECK(waitpid(pids[i], &status, 0) == pids[i], "wait client %d", i);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "client %d exited cleanly", i);
    }

    printf("bug-tcp-concurrent-connect: OK\n");
    return 0;
}
