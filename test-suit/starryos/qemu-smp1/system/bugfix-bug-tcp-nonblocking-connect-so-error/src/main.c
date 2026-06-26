#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#define PORT 19132

#define CHECK(cond, fmt, ...)                                                   \
    do {                                                                        \
        if (!(cond)) {                                                          \
            fprintf(stderr, "FAIL: " fmt " (errno=%d %s)\n", ##__VA_ARGS__,   \
                    errno, strerror(errno));                                    \
            return 1;                                                           \
        }                                                                       \
        printf("PASS: " fmt "\n", ##__VA_ARGS__);                             \
    } while (0)

static struct sockaddr_in loopback_addr(void) {
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons(PORT);
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    return addr;
}

static int poll_retry(struct pollfd *fds, nfds_t nfds, int timeout_ms) {
    for (;;) {
        int ret = poll(fds, nfds, timeout_ms);
        if (ret < 0 && errno == EINTR) {
            continue;
        }
        return ret;
    }
}

static ssize_t read_retry(int fd, void *buf, size_t len) {
    for (;;) {
        ssize_t ret = read(fd, buf, len);
        if (ret < 0 && errno == EINTR) {
            continue;
        }
        return ret;
    }
}

static ssize_t write_retry(int fd, const void *buf, size_t len) {
    for (;;) {
        ssize_t ret = write(fd, buf, len);
        if (ret < 0 && errno == EINTR) {
            continue;
        }
        return ret;
    }
}

static int accept_retry(int fd) {
    for (;;) {
        int ret = accept(fd, NULL, NULL);
        if (ret < 0 && errno == EINTR) {
            continue;
        }
        return ret;
    }
}

static pid_t waitpid_retry(pid_t pid, int *status, int options) {
    for (;;) {
        pid_t ret = waitpid(pid, status, options);
        if (ret < 0 && errno == EINTR) {
            continue;
        }
        return ret;
    }
}

static int make_listener(void) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;

    int one = 1;
    (void)setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));

    struct sockaddr_in addr = loopback_addr();
    if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        close(fd);
        return -1;
    }
    if (listen(fd, 4) < 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static void server_process(int ready_fd) {
    int listener = make_listener();
    if (listener < 0) _exit(10);
    if (write_retry(ready_fd, "R", 1) != 1) _exit(15);
    close(ready_fd);

    int fd = accept_retry(listener);
    if (fd < 0) _exit(11);

    char byte = 0;
    if (read_retry(fd, &byte, 1) != 1) _exit(12);
    if (byte != 'x') _exit(13);
    if (write_retry(fd, "y", 1) != 1) _exit(14);

    close(fd);
    close(listener);
    _exit(0);
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("=== bug-tcp-nonblocking-connect-so-error ===\n");

    int ready_pipe[2];
    CHECK(pipe(ready_pipe) == 0, "create server ready pipe");

    pid_t server = fork();
    if (server == 0) {
        close(ready_pipe[0]);
        server_process(ready_pipe[1]);
    }
    close(ready_pipe[1]);
    CHECK(server > 0, "fork server");

    struct pollfd ready_pfd;
    memset(&ready_pfd, 0, sizeof(ready_pfd));
    ready_pfd.fd = ready_pipe[0];
    ready_pfd.events = POLLIN;
    int ret = poll_retry(&ready_pfd, 1, 3000);
    CHECK(ret == 1 && (ready_pfd.revents & POLLIN) != 0,
          "server reports listener ready");

    char ready = 0;
    CHECK(read_retry(ready_pipe[0], &ready, 1) == 1 && ready == 'R',
          "read server ready signal");
    close(ready_pipe[0]);

    int fd = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(fd >= 0, "create client socket");

    int flags = fcntl(fd, F_GETFL, 0);
    CHECK(flags >= 0, "get client flags");
    CHECK(fcntl(fd, F_SETFL, flags | O_NONBLOCK) == 0, "set O_NONBLOCK");

    struct sockaddr_in addr = loopback_addr();
    ret = connect(fd, (struct sockaddr *)&addr, sizeof(addr));
    CHECK(ret == 0 || errno == EINPROGRESS, "nonblocking connect starts");

    struct pollfd pfd;
    memset(&pfd, 0, sizeof(pfd));
    pfd.fd = fd;
    pfd.events = POLLOUT;
    ret = poll_retry(&pfd, 1, 3000);
    CHECK(ret == 1, "poll reports connect completion");
    CHECK((pfd.revents & (POLLOUT | POLLERR | POLLHUP)) != 0,
          "poll returns output or error event");

    int so_error = -1;
    socklen_t so_error_len = sizeof(so_error);
    CHECK(getsockopt(fd, SOL_SOCKET, SO_ERROR, &so_error, &so_error_len) == 0,
          "getsockopt SO_ERROR succeeds");
    CHECK(so_error == 0, "SO_ERROR is zero after successful connect");

    errno = 0;
    ret = connect(fd, (struct sockaddr *)&addr, sizeof(addr));
    CHECK(ret == -1 && errno == EISCONN, "second connect reports EISCONN");

    CHECK(write_retry(fd, "x", 1) == 1, "write after nonblocking connect");
    memset(&pfd, 0, sizeof(pfd));
    pfd.fd = fd;
    pfd.events = POLLIN;
    ret = poll_retry(&pfd, 1, 3000);
    CHECK(ret == 1, "poll reports server reply");
    CHECK((pfd.revents & (POLLIN | POLLERR | POLLHUP)) != 0,
          "poll returns input or close event");

    char reply = 0;
    CHECK(read_retry(fd, &reply, 1) == 1 && reply == 'y', "read server reply");
    close(fd);

    int status = 0;
    CHECK(waitpid_retry(server, &status, 0) == server, "wait server");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "server exits cleanly");

    printf("bug-tcp-nonblocking-connect-so-error: OK\n");
    return 0;
}
