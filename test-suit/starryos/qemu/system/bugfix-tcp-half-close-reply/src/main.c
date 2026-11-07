#define _GNU_SOURCE

#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

static int passed;
static int failed;

static void check(int condition, const char *message)
{
    if (condition) {
        ++passed;
        printf("PASS: %s\n", message);
    } else {
        ++failed;
        printf("FAIL: %s\n", message);
    }
}

static const char request[] = "HEAD / HTTP/1.0\r\n\r\n";
static const char reply[] = "HTTP/1.0 301 Moved Permanently\r\nContent-Length: 0\r\n\r\n";

struct server {
    int listener;
    /* Delay before replying, so the client is already asleep in poll(). */
    int delay_ms;
    /* Close without reading or replying: the peer-FIN-only case. */
    int close_at_once;
    /* Keep the connection open until the client is done. */
    int hold;
    int conn;
};

static long elapsed_ms(const struct timespec *since)
{
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (now.tv_sec - since->tv_sec) * 1000 + (now.tv_nsec - since->tv_nsec) / 1000000;
}

/* Reads the request up to the client's FIN, replies, and closes: the order a
 * one-shot HTTP proxy answers `printf ... | nc host port` in. */
static void *serve(void *arg)
{
    struct server *server = arg;
    int conn = accept(server->listener, NULL, NULL);
    server->conn = conn;
    if (conn < 0 || server->hold) {
        return NULL;
    }
    if (!server->close_at_once) {
        char buf[256];
        while (read(conn, buf, sizeof(buf)) > 0) {
        }
        if (server->delay_ms > 0) {
            usleep(server->delay_ms * 1000);
        }
        if (write(conn, reply, strlen(reply)) < 0) {
            perror("server write");
        }
    }
    close(conn);
    return NULL;
}

static int open_listener(struct sockaddr_in *addr)
{
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    socklen_t len = sizeof(*addr);
    memset(addr, 0, sizeof(*addr));
    addr->sin_family = AF_INET;
    addr->sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (fd < 0 || bind(fd, (struct sockaddr *)addr, sizeof(*addr)) < 0 || listen(fd, 1) < 0
        || getsockname(fd, (struct sockaddr *)addr, &len) < 0) {
        perror("listener");
        return -1;
    }
    return fd;
}

static int start(struct server *server, pthread_t *thread)
{
    struct sockaddr_in addr;
    server->listener = open_listener(&addr);
    if (server->listener < 0 || pthread_create(thread, NULL, serve, server) != 0) {
        return -1;
    }
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0 || connect(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
        return -1;
    }
    return fd;
}

/* A holding server's thread has already been joined by the caller, which
 * needed its accepted connection; joining it twice is undefined. */
static void finish(int fd, struct server *server, pthread_t thread)
{
    close(fd);
    if (server->hold) {
        if (server->conn >= 0) {
            close(server->conn);
        }
    } else {
        pthread_join(thread, NULL);
    }
    close(server->listener);
}

static short wait_for(int fd, short events, int timeout_ms, long *waited)
{
    struct timespec start_at;
    clock_gettime(CLOCK_MONOTONIC, &start_at);
    struct pollfd pfd = { .fd = fd, .events = events };
    int ready = poll(&pfd, 1, timeout_ms);
    *waited = elapsed_ms(&start_at);
    return ready > 0 ? pfd.revents : 0;
}

/* What `printf ... | nc host port` does: send, half-close, wait, read. */
static void half_close_then_read(const char *name, int delay_ms)
{
    char label[200];
    struct server server = { .delay_ms = delay_ms };
    pthread_t thread;
    int fd = start(&server, &thread);
    check(fd >= 0, "connect");
    if (fd < 0) {
        return;
    }

    check(write(fd, request, strlen(request)) == (ssize_t)strlen(request), "send the request");
    check(shutdown(fd, SHUT_WR) == 0, "shut down the write side");

    long waited;
    short revents = wait_for(fd, POLLIN, 3000, &waited);
    snprintf(label, sizeof(label), "%s: poll reports the reply as readable (revents=%#x, %ld ms)",
             name, revents, waited);
    check(revents & POLLIN, label);

    char buf[256] = { 0 };
    ssize_t got = read(fd, buf, sizeof(buf) - 1);
    snprintf(label, sizeof(label), "%s: read returns the whole reply (%zd bytes)", name, got);
    check(got == (ssize_t)strlen(reply) && memcmp(buf, reply, strlen(reply)) == 0, label);

    ssize_t eof = read(fd, buf, sizeof(buf));
    snprintf(label, sizeof(label), "%s: the next read is end of file (%zd)", name, eof);
    check(eof == 0, label);

    /* Both directions are shut now: ours by shutdown(), theirs by the FIN. */
    revents = wait_for(fd, POLLIN, 0, &waited);
    snprintf(label, sizeof(label), "%s: both directions shut reports POLLHUP (revents=%#x)", name,
             revents);
    check(revents & POLLHUP, label);

    finish(fd, &server, thread);
}

/* The peer closes and we have shut nothing: readable end of file and
 * POLLRDHUP, but not POLLHUP, since our own direction is still open. */
static void peer_closes(void)
{
    char label[200];
    struct server server = { .close_at_once = 1 };
    pthread_t thread;
    int fd = start(&server, &thread);
    check(fd >= 0, "peer close: connect");
    if (fd < 0) {
        return;
    }
    pthread_join(thread, NULL);

    long waited;
    short revents = wait_for(fd, POLLIN | POLLRDHUP, 3000, &waited);
    snprintf(label, sizeof(label), "peer close: POLLIN and POLLRDHUP (revents=%#x, %ld ms)",
             revents, waited);
    check((revents & POLLIN) && (revents & POLLRDHUP), label);
    snprintf(label, sizeof(label), "peer close: no POLLHUP while our side is open (revents=%#x)",
             revents);
    check(!(revents & POLLHUP), label);

    char buf[16];
    check(read(fd, buf, sizeof(buf)) == 0, "peer close: read is end of file");
    close(fd);
    close(server.listener);
}

/* shutdown(SHUT_RD) on a quiet connection: reads end at once, and poll says
 * so, as Linux sets EPOLLIN for any receive shutdown. */
static void read_shutdown(void)
{
    char label[200];
    struct server server = { .hold = 1 };
    pthread_t thread;
    int fd = start(&server, &thread);
    check(fd >= 0, "read shutdown: connect");
    if (fd < 0) {
        return;
    }
    pthread_join(thread, NULL);

    check(shutdown(fd, SHUT_RD) == 0, "read shutdown: shut down the read side");
    long waited;
    short revents = wait_for(fd, POLLIN | POLLRDHUP, 3000, &waited);
    snprintf(label, sizeof(label), "read shutdown: POLLIN and POLLRDHUP (revents=%#x, %ld ms)",
             revents, waited);
    check((revents & POLLIN) && (revents & POLLRDHUP), label);

    char buf[16];
    ssize_t got = read(fd, buf, sizeof(buf));
    snprintf(label, sizeof(label), "read shutdown: read is end of file (%zd)", got);
    check(got == 0, label);

    revents = wait_for(fd, POLLOUT, 0, &waited);
    snprintf(label, sizeof(label), "read shutdown: still writable (revents=%#x)", revents);
    check(revents & POLLOUT, label);
    finish(fd, &server, thread);
}

/* Linux keeps what is already queued after shutdown(SHUT_RD): reads drain it
 * first and only then report end of file. */
static void read_shutdown_with_queued_data(void)
{
    char label[200];
    struct server server = { .hold = 1 };
    pthread_t thread;
    int fd = start(&server, &thread);
    check(fd >= 0, "queued read shutdown: connect");
    if (fd < 0) {
        return;
    }
    pthread_join(thread, NULL);
    check(write(server.conn, "queued", 6) == 6, "queued read shutdown: peer sends six bytes");

    long waited;
    short revents = wait_for(fd, POLLIN, 3000, &waited);
    check(revents & POLLIN, "queued read shutdown: the bytes arrive");
    check(shutdown(fd, SHUT_RD) == 0, "queued read shutdown: shut down the read side");

    char buf[16] = { 0 };
    ssize_t got = read(fd, buf, sizeof(buf));
    snprintf(label, sizeof(label), "queued read shutdown: read returns the queued bytes (%zd, errno %d)",
             got, got < 0 ? errno : 0);
    check(got == 6 && memcmp(buf, "queued", 6) == 0, label);
    got = read(fd, buf, sizeof(buf));
    snprintf(label, sizeof(label), "queued read shutdown: then end of file (%zd, errno %d)", got,
             got < 0 ? errno : 0);
    check(got == 0, label);
    finish(fd, &server, thread);
}

/* An unconnected socket is TCP_CLOSE with nothing shut down: writable and
 * hung up, never readable. */
static void unconnected(void)
{
    char label[200];
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    long waited;
    short revents = wait_for(fd, POLLIN | POLLOUT | POLLRDHUP, 0, &waited);
    snprintf(label, sizeof(label), "unconnected: POLLOUT and POLLHUP only (revents=%#x)", revents);
    check(revents == (POLLOUT | POLLHUP), label);
    close(fd);
}

int main(void)
{
    half_close_then_read("reply at once", 0);
    half_close_then_read("reply while the client waits", 200);
    peer_closes();
    read_shutdown();
    read_shutdown_with_queued_data();
    unconnected();

    printf("RESULT: %d passed / %d failed\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    printf("TEST FAILED\n");
    return 1;
}
