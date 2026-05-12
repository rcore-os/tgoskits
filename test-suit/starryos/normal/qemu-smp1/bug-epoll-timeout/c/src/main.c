/*
 * bug-epoll-timeout.c — Reproducer for StarryOS epoll_wait timeout starvation bug
 *
 * Root cause: When the console TTY uses Manual polling mode (no kernel-mode
 * interrupts), register_rx_waker() calls waker.wake_by_ref() immediately,
 * causing poll_io to busy-loop. The epoll_wait timeout is implemented via
 * select_biased! { poll_io, sleep_until }, but sleep_until depends on
 * check_timer_events() being called from the timer IRQ handler — which
 * can't fire while the CPU is busy-looping in kernel mode.
 *
 * Test: Call epoll_wait with a short timeout (50ms) on stdin (which has no
 * data). Measure how long it actually takes to return. On Linux, it should
 * return in ~50ms. On StarryOS with the bug, it stalls indefinitely.
 *
 * Pass criteria: all 20 iterations return in 40–200ms with n==0.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

static struct termios orig_termios;

static void restore_term(void) {
    tcsetattr(STDIN_FILENO, TCSAFLUSH, &orig_termios);
}

static void raw_mode(void) {
    tcgetattr(STDIN_FILENO, &orig_termios);
    atexit(restore_term);
    struct termios raw = orig_termios;
    raw.c_iflag &= ~(BRKINT | ICRNL | INPCK | ISTRIP | IXON);
    raw.c_oflag &= ~(OPOST);
    raw.c_cflag |= (CS8);
    raw.c_lflag &= ~(ECHO | ICANON | IEXTEN | ISIG);
    raw.c_cc[VMIN]  = 0;
    raw.c_cc[VTIME] = 0;
    tcsetattr(STDIN_FILENO, TCSAFLUSH, &raw);
}

static long elapsed_ms(struct timespec *start) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (now.tv_sec - start->tv_sec) * 1000L +
           (now.tv_nsec - start->tv_nsec) / 1000000;
}

int main(void) {
    raw_mode();

    int epfd = epoll_create1(0);
    if (epfd < 0) { perror("epoll_create1"); return 1; }

    struct epoll_event ev = { .events = EPOLLIN, .data.fd = STDIN_FILENO };
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, STDIN_FILENO, &ev) < 0) {
        perror("epoll_ctl");
        return 1;
    }

    const int timeout_ms = 50;
    const int iterations = 20;
    int failures = 0;

    for (int i = 0; i < iterations; i++) {
        struct timespec start;
        clock_gettime(CLOCK_MONOTONIC, &start);

        struct epoll_event events[1];
        int n = epoll_wait(epfd, events, 1, timeout_ms);

        long dt = elapsed_ms(&start);

        /* n must be 0 (timeout, no input), elapsed must be 40–200ms */
        int ok = (n == 0 && dt >= 40 && dt <= 200);
        if (!ok) failures++;

        printf("  iter %2d: epoll_wait returned %d in %3ld ms %s\n",
               i, n, dt, ok ? "[OK]" : "[FAIL]");
    }

    close(epfd);

    if (failures == 0) {
        printf("TEST PASSED\n");
        return 0;
    } else {
        printf("TEST FAILED: %d/%d iterations out of tolerance\n",
               failures, iterations);
        return 1;
    }
}
