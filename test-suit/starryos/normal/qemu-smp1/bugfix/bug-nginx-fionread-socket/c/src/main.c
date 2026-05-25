#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <unistd.h>

#ifndef FIONREAD
#define FIONREAD 0x541B
#endif

static int passed;
static int failed;

static void note_pass(const char *name)
{
    printf("PASS: %s\n", name);
    passed++;
}

static void note_fail(const char *name, const char *detail)
{
    printf("FAIL: %s: %s\n", name, detail);
    failed++;
}

static void close_fd(int fd)
{
    if (fd >= 0) {
        close(fd);
    }
}

static void on_alarm(int sig)
{
    (void)sig;
    fprintf(stderr, "FAIL: test timed out\n");
    _exit(124);
}

static void expect_fionread(int fd, int expected, const char *name)
{
    int available = -1;
    errno = 0;
    int ret = ioctl(fd, FIONREAD, &available);
    if (ret == 0 && available == expected) {
        note_pass(name);
        return;
    }

    char detail[192];
    snprintf(detail, sizeof(detail),
             "ioctl ret=%d errno=%d (%s) available=%d expected=%d", ret, errno,
             strerror(errno), available, expected);
    note_fail(name, detail);
}

static void expect_errno(int ret, int saved_errno, int expected_errno,
                         const char *name)
{
    if (ret == -1 && saved_errno == expected_errno) {
        note_pass(name);
        return;
    }

    char detail[192];
    snprintf(detail, sizeof(detail),
             "ret=%d errno=%d (%s), expected -1/%d (%s)", ret, saved_errno,
             strerror(saved_errno), expected_errno, strerror(expected_errno));
    note_fail(name, detail);
}

static int make_listener(struct sockaddr_in *addr)
{
    int listen_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (listen_fd < 0) {
        perror("socket(listen)");
        return -1;
    }

    int one = 1;
    if (setsockopt(listen_fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one)) < 0) {
        perror("setsockopt(SO_REUSEADDR)");
        close_fd(listen_fd);
        return -1;
    }

    memset(addr, 0, sizeof(*addr));
    addr->sin_family = AF_INET;
    addr->sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr->sin_port = 0;

    if (bind(listen_fd, (struct sockaddr *)addr, sizeof(*addr)) < 0) {
        perror("bind");
        close_fd(listen_fd);
        return -1;
    }
    if (listen(listen_fd, 1) < 0) {
        perror("listen");
        close_fd(listen_fd);
        return -1;
    }

    socklen_t len = sizeof(*addr);
    if (getsockname(listen_fd, (struct sockaddr *)addr, &len) < 0) {
        perror("getsockname");
        close_fd(listen_fd);
        return -1;
    }

    return listen_fd;
}

static int make_tcp_pair(int *client_fd, int *server_fd)
{
    *client_fd = -1;
    *server_fd = -1;

    struct sockaddr_in addr;
    int listen_fd = make_listener(&addr);
    if (listen_fd < 0) {
        return -1;
    }

    *client_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (*client_fd < 0) {
        perror("socket(client)");
        close_fd(listen_fd);
        return -1;
    }
    if (connect(*client_fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        perror("connect");
        close_fd(*client_fd);
        close_fd(listen_fd);
        *client_fd = -1;
        return -1;
    }

    *server_fd = accept(listen_fd, NULL, NULL);
    close_fd(listen_fd);
    if (*server_fd < 0) {
        perror("accept");
        close_fd(*client_fd);
        *client_fd = -1;
        return -1;
    }

    return 0;
}

static void expect_listener_fionread_einval(void)
{
    struct sockaddr_in addr;
    int listen_fd = make_listener(&addr);
    if (listen_fd < 0) {
        note_fail("listen socket setup", "make_listener failed");
        return;
    }

    int client_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (client_fd < 0) {
        note_fail("client socket for listen probe", strerror(errno));
        close_fd(listen_fd);
        return;
    }
    if (connect(client_fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        note_fail("queue connection on listen socket", strerror(errno));
        close_fd(client_fd);
        close_fd(listen_fd);
        return;
    }

    int available = -1;
    errno = 0;
    int ret = ioctl(listen_fd, FIONREAD, &available);
    expect_errno(ret, errno, EINVAL,
                 "FIONREAD on listening TCP socket returns EINVAL");

    close_fd(client_fd);
    close_fd(listen_fd);
}

static void wait_readable(int fd, const char *name)
{
    struct pollfd pfd;
    memset(&pfd, 0, sizeof(pfd));
    pfd.fd = fd;
    pfd.events = POLLIN;

    errno = 0;
    int ret = poll(&pfd, 1, 3000);
    if (ret == 1 && (pfd.revents & (POLLIN | POLLERR | POLLHUP)) != 0) {
        note_pass(name);
        return;
    }

    char detail[192];
    snprintf(detail, sizeof(detail), "poll ret=%d revents=0x%x errno=%d (%s)",
             ret, pfd.revents, errno, strerror(errno));
    note_fail(name, detail);
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    signal(SIGALRM, on_alarm);
    alarm(20);

    printf("=== bug-nginx-fionread-socket ===\n");

    expect_listener_fionread_einval();

    int client_fd = -1;
    int server_fd = -1;
    if (make_tcp_pair(&client_fd, &server_fd) != 0) {
        printf("STARRY_GROUPED_TEST_FAILED: bug-nginx-fionread-socket\n");
        return 1;
    }

    expect_fionread(server_fd, 0, "accepted TCP socket initially has no queued bytes");

    const char payload[] = "nginx-FIONREAD-payload";
    const int payload_len = (int)sizeof(payload) - 1;
    ssize_t written = write(client_fd, payload, (size_t)payload_len);
    if (written == payload_len) {
        note_pass("client writes payload");
    } else {
        char detail[160];
        snprintf(detail, sizeof(detail), "write returned %zd expected %d", written,
                 payload_len);
        note_fail("client writes payload", detail);
    }

    wait_readable(server_fd, "server socket becomes readable");
    expect_fionread(server_fd, payload_len, "FIONREAD reports full queued payload");

    char buf[8];
    ssize_t nread = read(server_fd, buf, sizeof(buf));
    if (nread == (ssize_t)sizeof(buf) &&
        memcmp(buf, payload, sizeof(buf)) == 0) {
        note_pass("server reads part of payload");
    } else {
        char detail[160];
        snprintf(detail, sizeof(detail), "read returned %zd expected %zu", nread,
                 sizeof(buf));
        note_fail("server reads part of payload", detail);
    }

    expect_fionread(server_fd, payload_len - (int)sizeof(buf),
                    "FIONREAD reports remaining payload");

    char rest[64];
    nread = read(server_fd, rest, sizeof(rest));
    if (nread == payload_len - (ssize_t)sizeof(buf)) {
        note_pass("server drains payload");
    } else {
        char detail[160];
        snprintf(detail, sizeof(detail), "read returned %zd expected %zd", nread,
                 payload_len - (ssize_t)sizeof(buf));
        note_fail("server drains payload", detail);
    }

    expect_fionread(server_fd, 0, "FIONREAD returns zero after draining payload");

    errno = 0;
    int ret = ioctl(server_fd, FIONREAD, NULL);
    expect_errno(ret, errno, EFAULT, "FIONREAD NULL pointer returns EFAULT");

    close_fd(client_fd);
    close_fd(server_fd);

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
        printf("STARRY_GROUPED_TEST_PASSED: bug-nginx-fionread-socket\n");
        return 0;
    }

    printf("TEST FAILED\n");
    printf("STARRY_GROUPED_TEST_FAILED: bug-nginx-fionread-socket\n");
    return 1;
}
