#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#define CLIENTS 8
#define PORT 19131

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

static int make_listener(void) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;

    int one = 1;
    (void)setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons(PORT);
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);

    if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        close(fd);
        return -1;
    }
    if (listen(fd, CLIENTS) < 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static void client_process(int idx) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) _exit(10);

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons(PORT);
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);

    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) _exit(11);

    char msg[32];
    int len = snprintf(msg, sizeof(msg), "client-%d", idx);
    if (len <= 0 || write(fd, msg, (size_t)len) != len) _exit(12);

    close(fd);
    _exit(0);
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    signal(SIGALRM, on_alarm);
    alarm(20);

    printf("=== bug-tcp-concurrent-connect ===\n");

    int listener = make_listener();
    CHECK(listener >= 0, "listen on 127.0.0.1:%d", PORT);

    pid_t pids[CLIENTS];
    for (int i = 0; i < CLIENTS; i++) {
        pid_t pid = fork();
        CHECK(pid >= 0, "fork client %d", i);
        if (pid == 0) {
            client_process(i);
        }
        pids[i] = pid;
    }

    int seen[CLIENTS] = {0};
    for (int i = 0; i < CLIENTS; i++) {
        int fd = accept(listener, NULL, NULL);
        CHECK(fd >= 0, "accept client %d", i);

        char buf[64];
        ssize_t n = read(fd, buf, sizeof(buf) - 1);
        close(fd);
        CHECK(n > 0, "read payload from accepted client %d", i);
        buf[n] = '\0';

        int idx = -1;
        CHECK(sscanf(buf, "client-%d", &idx) == 1, "parse payload '%s'", buf);
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

    close(listener);
    printf("bug-tcp-concurrent-connect: OK\n");
    return 0;
}
