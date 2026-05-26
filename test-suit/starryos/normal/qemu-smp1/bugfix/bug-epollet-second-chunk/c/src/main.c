/*
 * bug-epollet-second-chunk: an EPOLLET registration must report a second
 * chunk that arrives after the first chunk was fully drained.
 *
 * Two related bugs in the EPOLLET delivery path used to break this:
 *
 *   1. NoEvent path called check_and_register_waker(), which on a connected
 *      TCP socket immediately observed EPOLLOUT-ready in file.poll() and
 *      re-fired the waker.  That created a tight busy-loop that filled the
 *      ready_queue with phantom events and starved real wakeups.
 *
 *   2. EventAndRemove cleared the in-queue flag and then registered a new
 *      waker.  The previous InterestWaker had already been consumed by the
 *      wake that delivered the first chunk, so writes that arrived between
 *      mark_not_in_queue() and register_waker_only() hit an empty PollSet
 *      and the second chunk's wake was silently dropped.
 *
 * This test exercises the second-chunk path repeatedly to maximise the
 * chance of hitting the race window, and also verifies the basic NoEvent
 * path by polling with a 0 ms timeout on a connected socket without any
 * outstanding data.
 */

#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <unistd.h>

#define ROUNDS 32

static int make_loopback_pair(int *cli, int *srv)
{
    int listen_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (listen_fd < 0)
        return -1;
    int opt = 1;
    setsockopt(listen_fd, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));

    struct sockaddr_in addr = {
        .sin_family = AF_INET,
        .sin_addr.s_addr = htonl(INADDR_LOOPBACK),
        .sin_port = 0,
    };
    if (bind(listen_fd, (struct sockaddr *)&addr, sizeof(addr)) < 0
        || listen(listen_fd, 1) < 0) {
        close(listen_fd);
        return -1;
    }
    socklen_t len = sizeof(addr);
    if (getsockname(listen_fd, (struct sockaddr *)&addr, &len) < 0) {
        close(listen_fd);
        return -1;
    }

    int c = socket(AF_INET, SOCK_STREAM, 0);
    if (c < 0) {
        close(listen_fd);
        return -1;
    }
    if (connect(c, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        close(c);
        close(listen_fd);
        return -1;
    }
    int s = accept(listen_fd, NULL, NULL);
    close(listen_fd);
    if (s < 0) {
        close(c);
        return -1;
    }
    *cli = c;
    *srv = s;
    return 0;
}

static int drain(int fd)
{
    char buf[64];
    int total = 0;
    for (;;) {
        ssize_t n = read(fd, buf, sizeof(buf));
        if (n > 0) {
            total += (int)n;
            continue;
        }
        if (n < 0 && errno == EAGAIN)
            return total;
        return -1;
    }
}

int main(void)
{
    int cli, srv;
    if (make_loopback_pair(&cli, &srv) < 0) {
        fprintf(stderr, "loopback pair setup failed: %s\n", strerror(errno));
        return 1;
    }

    int flags = fcntl(cli, F_GETFL, 0);
    fcntl(cli, F_SETFL, flags | O_NONBLOCK);

    int epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        fprintf(stderr, "epoll_create1: %s\n", strerror(errno));
        return 1;
    }
    struct epoll_event ev = {
        .events = EPOLLIN | EPOLLET,
        .data.fd = cli,
    };
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, cli, &ev) < 0) {
        fprintf(stderr, "epoll_ctl: %s\n", strerror(errno));
        return 1;
    }

    /*
     * Bug 1: a connected TCP socket has EPOLLOUT always ready.  With the
     * NoEvent path's old check_and_register_waker, any spurious wake would
     * find OUT in file.poll() and re-queue the interest, returning phantom
     * events here.  After the fix, epoll_wait must time out cleanly.
     */
    struct epoll_event evs[8];
    int n = epoll_wait(epfd, evs, 8, 50);
    if (n != 0) {
        fprintf(stderr, "STARRY_GROUPED_TEST_FAILED: bug-epollet-second-chunk "
                        "(phantom events from idle socket: n=%d)\n", n);
        return 1;
    }

    /*
     * Bug 2: send a chunk, drain it, send another chunk back-to-back.
     * Repeat to maximise the chance of writing in the race window between
     * mark_not_in_queue() and the fresh register_waker_only() install.
     */
    for (int i = 0; i < ROUNDS; i++) {
        char msg[16];
        int len = snprintf(msg, sizeof(msg), "round-%d-A", i);
        if (write(srv, msg, (size_t)len) != len) {
            fprintf(stderr, "write A failed: %s\n", strerror(errno));
            return 1;
        }
        n = epoll_wait(epfd, evs, 8, 1000);
        if (n != 1) {
            fprintf(stderr,
                    "STARRY_GROUPED_TEST_FAILED: bug-epollet-second-chunk "
                    "(round=%d chunk A: epoll_wait returned %d, expected 1)\n",
                    i, n);
            return 1;
        }
        if (drain(cli) <= 0) {
            fprintf(stderr, "drain A failed: %s\n", strerror(errno));
            return 1;
        }

        len = snprintf(msg, sizeof(msg), "round-%d-B", i);
        if (write(srv, msg, (size_t)len) != len) {
            fprintf(stderr, "write B failed: %s\n", strerror(errno));
            return 1;
        }
        n = epoll_wait(epfd, evs, 8, 1000);
        if (n != 1) {
            fprintf(stderr,
                    "STARRY_GROUPED_TEST_FAILED: bug-epollet-second-chunk "
                    "(round=%d chunk B: epoll_wait returned %d, expected 1)\n",
                    i, n);
            return 1;
        }
        if (drain(cli) <= 0) {
            fprintf(stderr, "drain B failed: %s\n", strerror(errno));
            return 1;
        }
    }

    close(epfd);
    close(cli);
    close(srv);

    printf("STARRY_GROUPED_TEST_PASSED: bug-epollet-second-chunk\n");
    return 0;
}
