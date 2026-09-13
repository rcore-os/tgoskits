#define _GNU_SOURCE
#include "test_framework.h"

#include <fcntl.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <unistd.h>

static int watch(int fd, uint32_t events)
{
    int epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0)
        return -1;
    struct epoll_event event = {.events = events, .data.fd = fd};
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, fd, &event) != 0) {
        close(epfd);
        return -1;
    }
    return epfd;
}

static int wait_events(int epfd, int timeout_ms, uint32_t *events)
{
    struct epoll_event event = {0};
    int n = epoll_wait(epfd, &event, 1, timeout_ms);
    if (events)
        *events = n == 1 ? event.events : 0;
    return n;
}

static void drain_edges(int epfd)
{
    for (int i = 0; i < 8 && wait_events(epfd, 0, NULL) > 0; i++)
        ;
}

static void set_nonblocking(int fd)
{
    fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK);
}

/*
 * An edge-triggered reactor on one end of a stream socketpair must see an edge
 * only when that end's own readiness changes. Linux wakes the peer on send and
 * the writer when the peer frees buffer space; if both ends share one wait set,
 * every send and recv also re-arms the caller's own edges and the reactor spins.
 */
static void test_io_wakes_only_the_peer(void)
{
    int sv[2];
    CHECK_RET(socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv), 0, "create stream socketpair");
    int a = sv[0], b = sv[1];

    int a_out = watch(a, EPOLLOUT | EPOLLET);
    int b_in = watch(b, EPOLLIN | EPOLLET);
    int b_out = watch(b, EPOLLOUT | EPOLLET);
    CHECK(a_out >= 0 && b_in >= 0 && b_out >= 0, "create edge-triggered watches");

    CHECK_RET(wait_events(a_out, 0, NULL), 1, "writer reports its initial writable edge");
    CHECK_RET(wait_events(b_out, 0, NULL), 1, "peer reports its initial writable edge");
    CHECK_RET(wait_events(b_in, 0, NULL), 0, "peer has nothing to read yet");

    CHECK_RET(write(a, "ping", 4), 4, "writer sends four bytes");

    uint32_t events = 0;
    CHECK_RET(wait_events(b_in, 1000, &events), 1, "peer gets a readable edge");
    CHECK(events & EPOLLIN, "peer edge carries EPOLLIN");
    CHECK_RET(wait_events(a_out, 0, NULL), 0, "send does not re-arm the writer's own writable edge");
    CHECK_RET(wait_events(b_out, 0, NULL), 0, "incoming data does not re-arm the peer's writable edge");

    char buf[4];
    CHECK_RET(read(b, buf, sizeof(buf)), 4, "peer reads the four bytes");
    CHECK_RET(wait_events(b_out, 0, NULL), 0, "recv does not re-arm the reader's own writable edge");

    close(b_out);
    close(b_in);
    close(a_out);
    close(a);
    close(b);
}

/* Freeing buffer space must still wake the blocked writer through the peer. */
static void test_peer_recv_wakes_full_writer(void)
{
    int sv[2];
    CHECK_RET(socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv), 0, "create stream socketpair");
    int a = sv[0], b = sv[1];
    set_nonblocking(a);
    set_nonblocking(b);

    int a_out = watch(a, EPOLLOUT | EPOLLET);
    CHECK(a_out >= 0, "watch the writer for writable edges");

    char block[4096];
    memset(block, 0x5a, sizeof(block));
    long sent = 0;
    for (;;) {
        ssize_t n = write(a, block, sizeof(block));
        if (n <= 0)
            break;
        sent += n;
    }
    CHECK(sent > 0 && errno == EAGAIN, "fill the writer until it would block");
    drain_edges(a_out);

    long got = 0;
    for (;;) {
        ssize_t n = read(b, block, sizeof(block));
        if (n <= 0)
            break;
        got += n;
    }
    CHECK(got == sent, "peer drains everything that was sent");

    uint32_t events = 0;
    CHECK_RET(wait_events(a_out, 1000, &events), 1, "draining wakes the writer");
    CHECK(events & EPOLLOUT, "writer edge carries EPOLLOUT");

    close(a_out);
    close(a);
    close(b);
}

/* Closing the write half must reach the peer's waiters. */
static void test_shutdown_wakes_peer(void)
{
    int sv[2];
    CHECK_RET(socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv), 0, "create stream socketpair");
    int a = sv[0], b = sv[1];

    int b_hup = watch(b, EPOLLIN | EPOLLRDHUP | EPOLLET);
    CHECK(b_hup >= 0, "watch the peer for hangup");
    CHECK_RET(wait_events(b_hup, 0, NULL), 0, "peer starts with no events");

    CHECK_RET(shutdown(a, SHUT_WR), 0, "writer shuts down its write half");
    uint32_t events = 0;
    CHECK_RET(wait_events(b_hup, 1000, &events), 1, "peer gets an edge for the shutdown");
    CHECK(events & EPOLLRDHUP, "peer edge carries EPOLLRDHUP");

    close(b_hup);
    close(a);
    close(b);
}

int main(void)
{
    TEST_START("unix stream socket I/O wakes only the peer endpoint");
    test_io_wakes_only_the_peer();
    test_peer_recv_wakes_full_writer();
    test_shutdown_wakes_peer();
    TEST_DONE();
}
