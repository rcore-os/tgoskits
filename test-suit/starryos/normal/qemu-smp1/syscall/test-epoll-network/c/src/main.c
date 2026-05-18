#define _POSIX_C_SOURCE 200809L
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <time.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <string.h>
#include <sys/wait.h>

/* usleep replacement using pure POSIX nanosleep */
static void msleep(long ms) {
    struct timespec ts = { .tv_sec = ms / 1000, .tv_nsec = (ms % 1000) * 1000000L };
    nanosleep(&ts, NULL);
}

#define PORT 12345

int total_failures = 0;
int total_tests = 5;

void set_nonblocking(int fd) {
    int flags = fcntl(fd, F_GETFL, 0);
    fcntl(fd, F_SETFL, flags | O_NONBLOCK);
}

void fail(const char* msg) {
    printf("[FAIL] %s\n", msg);
    total_failures++;
}

void pass(const char* msg) {
    printf("[PASS] %s\n", msg);
}

/* Drain the socket buffer between tests so failures don't cascade */
void drain_socket(int fd) {
    char temp[128];
    while (recv(fd, temp, sizeof(temp), 0) > 0)
        ;
}

int main(void) {
    printf("=== Starting Comprehensive Network/Epoll Tokio-Compat Suite ===\n\n");

    int epfd = epoll_create1(0);
    if (epfd < 0) {
        printf("[FATAL] epoll_create1 failed\n");
        return 1;
    }

    int rx_sock = socket(AF_INET, SOCK_DGRAM, 0);
    int tx_sock = socket(AF_INET, SOCK_DGRAM, 0);
    if (rx_sock < 0 || tx_sock < 0) {
        printf("[FATAL] Failed to create sockets\n");
        return 1;
    }

    set_nonblocking(rx_sock);

    struct sockaddr_in addr = {
        .sin_family = AF_INET,
        .sin_port   = htons(PORT),
    };
    addr.sin_addr.s_addr = inet_addr("127.0.0.1");

    if (bind(rx_sock, (struct sockaddr*)&addr, sizeof(addr)) < 0) {
        printf("[FATAL] Failed to bind rx_sock. Does your OS support loopback networking?\n");
        return 1;
    }

    struct epoll_event ev = { .events = EPOLLIN, .data.fd = rx_sock };
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, rx_sock, &ev) < 0) {
        printf("[FATAL] epoll_ctl ADD failed\n");
        return 1;
    }

    struct epoll_event events[2];
    int n;
    char buf[128];

    /* -----------------------------------------------------------------------
     * TEST 1: Network Stack Waker (smoltcp -> epoll)
     * --------------------------------------------------------------------- */
    printf("[TEST 1] Testing basic network packet wakeup...\n");
    drain_socket(rx_sock);

    pid_t pid1 = fork();
    if (pid1 == 0) {
        sleep(1);
        sendto(tx_sock, "A", 1, 0, (struct sockaddr*)&addr, sizeof(addr));
        exit(0);
    }

    n = epoll_wait(epfd, events, 1, 2000);
    if (n == 1 && events[0].data.fd == rx_sock) {
        pass("Network packet correctly woke epoll_wait.");
    } else {
        fail("epoll_wait timed out. Your network stack is not waking the PollSet!");
    }
    waitpid(pid1, NULL, 0);

    /* -----------------------------------------------------------------------
     * TEST 2: The EWOULDBLOCK / EAGAIN Contract
     * --------------------------------------------------------------------- */
    printf("\n[TEST 2] Testing non-blocking EAGAIN semantics...\n");
    drain_socket(rx_sock);

    int bytes = recv(rx_sock, buf, sizeof(buf), 0);
    if (bytes == -1 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
        pass("Empty non-blocking socket returned EAGAIN correctly.");
    } else if (bytes == 0) {
        fail("Empty socket returned 0! Tokio will think the connection closed (EOF).");
    } else {
        fail("Empty socket did not return EAGAIN.");
    }

    /* -----------------------------------------------------------------------
     * TEST 3: EPOLL_CTL_MOD State Preservation
     * --------------------------------------------------------------------- */
    printf("\n[TEST 3] Testing EPOLL_CTL_MOD...\n");
    ev.events = EPOLLOUT;
    if (epoll_ctl(epfd, EPOLL_CTL_MOD, rx_sock, &ev) < 0) {
        fail("EPOLL_CTL_MOD failed to execute.");
    } else {
        n = epoll_wait(epfd, events, 1, 100);
        if (n == 1 && (events[0].events & EPOLLOUT)) {
            pass("EPOLL_CTL_MOD correctly updated interest to EPOLLOUT.");
        } else {
            fail("EPOLL_CTL_MOD broke the wait queue or failed to wake on EPOLLOUT.");
        }
    }

    /* Restore to EPOLLIN for remaining tests */
    ev.events = EPOLLIN;
    epoll_ctl(epfd, EPOLL_CTL_MOD, rx_sock, &ev);

    /* -----------------------------------------------------------------------
     * TEST 4: Partial Drain (Level-Triggered Re-arm)
     * --------------------------------------------------------------------- */
    printf("\n[TEST 4] Testing Level-Triggered partial drain...\n");
    drain_socket(rx_sock);
    /* Send two separate datagrams so the second remains after reading the first */
    sendto(tx_sock, "B", 1, 0, (struct sockaddr*)&addr, sizeof(addr));
    sendto(tx_sock, "C", 1, 0, (struct sockaddr*)&addr, sizeof(addr));
    msleep(100); /* Give network stack time to process */

    n = epoll_wait(epfd, events, 1, 500);
    if (n > 0) {
        recv(rx_sock, buf, sizeof(buf), 0); /* Read datagram "B"; "C" remains */
        n = epoll_wait(epfd, events, 1, 500); /* Should wake immediately */
        if (n == 1) {
            pass("Level-triggered epoll correctly woke up again for unread bytes.");
        } else {
            fail("Level-triggered epoll failed to re-arm when buffer still had data.");
        }
    } else {
        fail("Initial epoll_wait failed, skipping partial drain check.");
    }

    /* -----------------------------------------------------------------------
     * TEST 5: Edge-Triggered (EPOLLET) Isolation
     * --------------------------------------------------------------------- */
    printf("\n[TEST 5] Testing Edge-Triggered (EPOLLET) semantics...\n");
    drain_socket(rx_sock);
    ev.events = EPOLLIN | EPOLLET;
    epoll_ctl(epfd, EPOLL_CTL_MOD, rx_sock, &ev);

    sendto(tx_sock, "D", 1, 0, (struct sockaddr*)&addr, sizeof(addr));
    msleep(100);

    n = epoll_wait(epfd, events, 1, 500); /* 1st wait: edge fires */
    if (n != 1) {
        fail("EPOLLET failed to wake on new data.");
    } else {
        /* Do NOT read the data — second wait must NOT fire */
        n = epoll_wait(epfd, events, 1, 500);
        if (n == 0) {
            pass("EPOLLET correctly ignored existing data (did not multi-fire).");
        } else {
            fail("EPOLLET behaved like Level-Triggered! It woke up without new data arriving.");
        }
    }

    /* -----------------------------------------------------------------------
     * SUMMARY
     * --------------------------------------------------------------------- */
    printf("\n=== Test Suite Complete ===\n");
    printf("Passed: %d / %d\n", total_tests - total_failures, total_tests);
    if (total_failures > 0) {
        printf("Result: FAILED\n");
        return 1;
    } else {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
}
