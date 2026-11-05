/*
 * EPOLL_CTL_ADD of a nested epoll while the epolls it watches are closed.
 *
 * Adding epoll C to epoll P walks the epolls C watches under the global
 * topology lock, to reject loops and bound the nesting depth. Another thread
 * closes those child epolls while the walk runs, so the walk can hold the last
 * reference to one of them. Every child watches a pipe read end whose wait
 * lock other threads keep busy, so destroying a child under the topology lock
 * would sleep with preemption disabled. The add must succeed, loop detection
 * must still see the new link, and the kernel must keep running.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <poll.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/epoll.h>
#include <time.h>
#include <unistd.h>

/* Child epolls watched by C in each round, and threads polling the pipe. */
#define CHILDREN 64
#define PIPE_POLLERS 2
/* Rounds of add-and-close; the time limit bounds slow emulation. */
#define ADD_ROUNDS 64
#define ADD_SECONDS 5

static int pipe_rd;
static int children[CHILDREN];
static uint64_t go;
static uint64_t closed;
static int stop;

static void *pipe_poller(void *arg) {
    (void)arg;
    struct pollfd pfd = {.fd = pipe_rd, .events = POLLIN};
    while (!__atomic_load_n(&stop, __ATOMIC_RELAXED)) {
        poll(&pfd, 1, 0);
    }
    return NULL;
}

/* Closes the children of each round, in the order C walks them. */
static void *closer(void *arg) {
    (void)arg;
    uint64_t seen = 0;
    for (;;) {
        uint64_t round;
        while ((round = __atomic_load_n(&go, __ATOMIC_ACQUIRE)) == seen) {
        }
        if (round == UINT64_MAX) {
            return NULL;
        }
        for (int i = 0; i < CHILDREN; i++) {
            close(children[i]);
        }
        seen = round;
        __atomic_store_n(&closed, round, __ATOMIC_RELEASE);
    }
}

static int add_interest(int epfd, int fd) {
    struct epoll_event ev = {.events = EPOLLIN, .data.fd = fd};
    return epoll_ctl(epfd, EPOLL_CTL_ADD, fd, &ev);
}

static long elapsed_ns(const struct timespec *start) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (now.tv_sec - start->tv_sec) * 1000000000L + (now.tv_nsec - start->tv_nsec);
}

static int add_while_closing(void) {
    int failures = 0;
    int rounds = 0;
    struct timespec start;
    clock_gettime(CLOCK_MONOTONIC, &start);
    for (; rounds < ADD_ROUNDS && elapsed_ns(&start) < ADD_SECONDS * 1000000000L; rounds++) {
        int c = epoll_create1(0);
        int p = epoll_create1(0);
        if (c < 0 || p < 0) {
            printf("round %d: epoll_create1 failed errno=%d\n", rounds, errno);
            return failures + 1;
        }
        for (int i = 0; i < CHILDREN; i++) {
            children[i] = epoll_create1(0);
            if (children[i] < 0 || add_interest(children[i], pipe_rd) != 0 ||
                add_interest(c, children[i]) != 0) {
                printf("round %d: child %d setup failed errno=%d\n", rounds, i, errno);
                return failures + 1;
            }
        }

        uint64_t round = (uint64_t)rounds + 1;
        __atomic_store_n(&go, round, __ATOMIC_RELEASE);
        if (add_interest(p, c) != 0) {
            printf("round %d: EPOLL_CTL_ADD of the nested epoll failed errno=%d\n", rounds,
                   errno);
            failures++;
        }
        while (__atomic_load_n(&closed, __ATOMIC_ACQUIRE) != round) {
        }
        if (add_interest(c, p) == 0 || errno != ELOOP) {
            printf("round %d: EPOLL_CTL_ADD closing a loop did not fail with ELOOP errno=%d\n",
                   rounds, errno);
            failures++;
        }
        close(p);
        close(c);
        if (failures != 0) {
            break;
        }
    }
    printf("added %d nested epolls while closing their children\n", rounds);
    return failures;
}

int main(void) {
    int pipefd[2];
    if (pipe(pipefd) != 0) {
        printf("pipe failed errno=%d\n", errno);
        return 1;
    }
    /* The write end stays open and unused: the children never become ready. */
    pipe_rd = pipefd[0];

    pthread_t threads[PIPE_POLLERS + 1];
    for (int i = 0; i < PIPE_POLLERS + 1; i++) {
        if (pthread_create(&threads[i], NULL, i == 0 ? closer : pipe_poller, NULL) != 0) {
            printf("pthread_create failed\n");
            return 1;
        }
    }

    int failures = add_while_closing();

    __atomic_store_n(&go, UINT64_MAX, __ATOMIC_RELEASE);
    __atomic_store_n(&stop, 1, __ATOMIC_RELAXED);
    for (int i = 0; i < PIPE_POLLERS + 1; i++) {
        pthread_join(threads[i], NULL);
    }
    close(pipefd[0]);
    close(pipefd[1]);
    if (failures != 0) {
        return 1;
    }
    printf("STARRY_EPOLL_NESTED_ADD_CLOSE_OK\n");
    return 0;
}
