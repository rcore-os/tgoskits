#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <unistd.h>

#define CHECK(condition) do { \
    if (!(condition)) { \
        fprintf(stderr, "FAIL: line %d: %s (errno=%d)\n", __LINE__, #condition, errno); \
        exit(EXIT_FAILURE); \
    } \
} while (0)

static void datagram_close(void)
{
    int pair[2];
    char byte = 0;
    CHECK(socketpair(AF_UNIX, SOCK_DGRAM | SOCK_NONBLOCK, 0, pair) == 0);
    CHECK(send(pair[1], "x", 1, 0) == 1);
    CHECK(close(pair[1]) == 0);
    struct pollfd ready = {.fd = pair[0], .events = POLLIN | POLLRDHUP};
    CHECK(poll(&ready, 1, 0) == 1);
    CHECK(ready.revents == POLLIN);
    CHECK(recv(pair[0], &byte, 1, 0) == 1 && byte == 'x');
    CHECK(poll(&ready, 1, 0) == 0);
    errno = 0;
    CHECK(recv(pair[0], &byte, 1, 0) == -1 && errno == EAGAIN);
    CHECK(close(pair[0]) == 0);
}

static void *close_peer(void *argument)
{
    int fd = *(int *)argument;
    /* Give the syscall time to register; host tests check wake ordering without
     * timing. Closing before registration must also remain level-triggered. */
    usleep(50000);
    CHECK(close(fd) == 0);
    return NULL;
}

static void seqpacket_close(short interest, int use_epoll)
{
    int pair[2];
    CHECK(socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_NONBLOCK, 0, pair) == 0);
    int epoll_fd = -1;
    if (use_epoll) {
        epoll_fd = epoll_create1(EPOLL_CLOEXEC);
        CHECK(epoll_fd >= 0);
        struct epoll_event event = {.events = (uint32_t)interest, .data.fd = pair[0]};
        CHECK(epoll_ctl(epoll_fd, EPOLL_CTL_ADD, pair[0], &event) == 0);
    }
    pthread_t closer;
    CHECK(pthread_create(&closer, NULL, close_peer, &pair[1]) == 0);
    for (int repetition = 0; repetition < 2; ++repetition) {
        uint32_t events;
        if (use_epoll) {
            struct epoll_event event = {0};
            CHECK(epoll_wait(epoll_fd, &event, 1, 2000) == 1);
            CHECK(event.data.fd == pair[0]);
            events = event.events;
        } else {
            struct pollfd ready = {.fd = pair[0], .events = interest};
            CHECK(poll(&ready, 1, 2000) == 1);
            events = (uint32_t)ready.revents;
        }
        CHECK(events & POLLHUP);
        CHECK(!(interest & POLLRDHUP) || (events & POLLRDHUP));
    }
    CHECK(pthread_join(closer, NULL) == 0);
    char byte;
    CHECK(recv(pair[0], &byte, 1, 0) == 0);
    CHECK(close(pair[0]) == 0);
    if (epoll_fd >= 0)
        CHECK(close(epoll_fd) == 0);
}

int main(void)
{
    datagram_close();
    for (int use_epoll = 0; use_epoll <= 1; ++use_epoll) {
        seqpacket_close(POLLRDHUP, use_epoll);
        seqpacket_close(POLLHUP, use_epoll);
    }
    puts("test-unix-peer-close: all tests passed");
    return EXIT_SUCCESS;
}
