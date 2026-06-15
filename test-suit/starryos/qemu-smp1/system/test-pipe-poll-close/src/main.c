// SPDX-License-Identifier: Apache-2.0
// Focused regression: verify that poll/epoll correctly detects a pipe close
// when the write end is dropped (child process exits).
//
// Mimics the Nix build monitoring pattern: Nix creates a pipe, forks a
// builder, and monitors the read end via epoll.  When the builder exits,
// epoll must return EPOLLIN|EPOLLHUP so Nix can detect completion.

#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/wait.h>
#include <unistd.h>

static int tests_pass;
static int tests_fail;

#define TEST(cond, msg)                                                        \
    do {                                                                      \
        if (cond) {                                                           \
            tests_pass++;                                                     \
            printf("  PASS: %s\n", msg);                                      \
        } else {                                                              \
            tests_fail++;                                                     \
            printf("  FAIL: %s (%s:%d)\n", msg, __FILE__, __LINE__);          \
        }                                                                     \
    } while (0)


// ─── Test 1: poll detects pipe close (HUP) ──────────────────────────────
static void test_poll_pipe_close(void) {
    printf("Test 1: poll detects pipe close when write end drops\n");
    int pipefd[2];
    TEST(pipe(pipefd) == 0, "pipe created");

    pid_t child = fork();
    TEST(child >= 0, "fork succeeded");

    if (child == 0) {
        // Child: close read end, write, then exit
        close(pipefd[0]);
        const char *msg = "hello";
        (void)!write(pipefd[1], msg, strlen(msg));
        close(pipefd[1]);
        _exit(0);
    }

    // Parent: close write end, wait for child to finish writing
    close(pipefd[1]);

    // Ensure child has written before polling, avoiding arch-dependent
    // scheduler race where poll(200ms) fires before the child is scheduled.
    waitpid(child, NULL, 0);

    // First read the data the child wrote
    char buf[64] = {0};
    struct pollfd pfd = {.fd = pipefd[0], .events = POLLIN};
    int ret = poll(&pfd, 1, 200);
    TEST(ret == 1, "poll returned 1 after child wrote data");
    TEST(pfd.revents & POLLIN, "POLLIN set after child writes");

    ssize_t n = read(pipefd[0], buf, sizeof(buf) - 1);
    TEST(n == 5, "read got 5 bytes from pipe");
    TEST(strcmp(buf, "hello") == 0, "read correct data");

    // Now poll again — pipe should show POLLHUP since write end is closed
    // and no data remains
    pfd.revents = 0;
    ret = poll(&pfd, 1, 200);
    TEST(ret == 1, "poll returned 1 after pipe close");
    TEST((pfd.revents & (POLLIN | POLLHUP)) != 0,
         "POLLIN or POLLHUP set after pipe close (Linux: both)");
    TEST(pfd.revents & POLLHUP, "POLLHUP set after pipe close");

    // Per Linux behaviour, a closed empty pipe should also set POLLIN
    // because read() would return 0 (EOF) without blocking.
    fprintf(stderr, "  INFO: revents=0x%x (POLLIN=0x%x POLLHUP=0x%x)\n",
            pfd.revents, POLLIN, POLLHUP);

    // Verify EOF
    n = read(pipefd[0], buf, sizeof(buf));
    TEST(n == 0, "read returns 0 (EOF) after pipe close");

    close(pipefd[0]);
    waitpid(child, NULL, 0);
}

// ─── Test 2: epoll detects pipe close ───────────────────────────────────
static void test_epoll_pipe_close(void) {
    printf("Test 2: epoll detects pipe close when write end drops\n");
    int pipefd[2];
    TEST(pipe(pipefd) == 0, "pipe created");

    int epfd = epoll_create1(0);
    TEST(epfd >= 0, "epoll_create1 succeeded");

    pid_t child = fork();
    TEST(child >= 0, "fork succeeded");

    if (child == 0) {
        // Child: close read end, write, then exit
        close(pipefd[0]);
        const char *msg = "world";
        (void)!write(pipefd[1], msg, strlen(msg));
        close(pipefd[1]);
        _exit(0);
    }

    // Parent: close write end, add read end to epoll
    close(pipefd[1]);

    struct epoll_event ev = {.events = EPOLLIN, .data.fd = pipefd[0]};
    TEST(epoll_ctl(epfd, EPOLL_CTL_ADD, pipefd[0], &ev) == 0,
         "epoll_ctl ADD succeeded");

    // Wait for first event: data available
    struct epoll_event events[4];
    int nfds = epoll_wait(epfd, events, 4, 200);
    TEST(nfds == 1, "epoll_wait returned 1 event when data is ready");
    TEST(events[0].data.fd == pipefd[0], "event is for pipe fd");
    TEST(events[0].events & EPOLLIN, "EPOLLIN set after child writes data");

    // Read data
    char buf[64] = {0};
    ssize_t n = read(pipefd[0], buf, sizeof(buf) - 1);
    TEST(n == 5, "read 5 bytes via epoll wake");
    TEST(strcmp(buf, "world") == 0, "correct data via epoll");

    // Now wait for close event — after draining data and child exited,
    // epoll should report EPOLLHUP
    nfds = epoll_wait(epfd, events, 4, 200);
    TEST(nfds == 1, "epoll_wait returned event after pipe close");
    TEST(events[0].data.fd == pipefd[0], "close event is for pipe fd");
    fprintf(stderr, "  INFO: epoll events=0x%x (EPOLLIN=0x%x EPOLLHUP=0x%x)\n",
            events[0].events, EPOLLIN, EPOLLHUP);

    // Linux: EPOLLHUP is set; EPOLLIN may or may not be set depending on
    // kernel version.  epoll_wait returning at all for the close is the
    // essential behaviour.
    TEST((events[0].events & (EPOLLIN | EPOLLHUP)) != 0,
         "EPOLLIN or EPOLLHUP set after pipe close");
    TEST(events[0].events & EPOLLHUP, "EPOLLHUP set after pipe close");

    // Verify EOF
    n = read(pipefd[0], buf, sizeof(buf));
    TEST(n == 0, "read returns 0 (EOF) after pipe close");

    close(pipefd[0]);
    close(epfd);
    waitpid(child, NULL, 0);
}

// ─── Test 3: epoll EPOLLIN-only interest still receives close event ─────
static void test_epoll_in_only_detects_close(void) {
    printf("Test 3: epoll EPOLLIN-only detects pipe close (Nix pattern)\n");
    int pipefd[2];
    TEST(pipe(pipefd) == 0, "pipe created");

    int epfd = epoll_create1(0);
    TEST(epfd >= 0, "epoll_create1 succeeded");


    pid_t child = fork();
    TEST(child >= 0, "fork succeeded");

    if (child == 0) {
        // Child: close read end, write marker, exit
        close(pipefd[0]);
        (void)!write(pipefd[1], "x", 1);
        (void)!fsync(pipefd[1]);
        close(pipefd[1]);
        _exit(0);
    }

    // Parent: close write end, monitor read end with EPOLLIN only
    close(pipefd[1]);

    struct epoll_event ev = {.events = EPOLLIN, .data.fd = pipefd[0]};
    TEST(epoll_ctl(epfd, EPOLL_CTL_ADD, pipefd[0], &ev) == 0,
         "epoll_ctl ADD with EPOLLIN only");

    // First event: data
    struct epoll_event events[4];
    int nfds = epoll_wait(epfd, events, 4, 200);
    TEST(nfds == 1, "EPOLLIN-only: first epoll_wait got data event");
    TEST(events[0].events & EPOLLIN, "EPOLLIN-only: EPOLLIN set for data");

    // Drain data
    char c;
    TEST(read(pipefd[0], &c, 1) == 1, "EPOLLIN-only: read 1 byte");

    // Now the child has exited and closed its write end.
    // We monitor with EPOLLIN only — this is exactly what Nix does.
    // The epoll must wake us up even though we only asked for EPOLLIN.
    // Verify the child has exited
    int status;
    waitpid(child, &status, 0);

    nfds = epoll_wait(epfd, events, 4, 200);
    // We must get at least 1 event — the pipe close.
    TEST(nfds >= 1, "EPOLLIN-only: epoll_wait returned after pipe close");
    if (nfds >= 1) {
        fprintf(stderr,
                "  INFO: EPOLLIN-only close events=0x%x (EPOLLIN=0x%x "
                "EPOLLHUP=0x%x)\n",
                events[0].events, EPOLLIN, EPOLLHUP);
        TEST((events[0].events & (EPOLLIN | EPOLLHUP)) != 0,
             "EPOLLIN-only: got EPOLLIN or EPOLLHUP on close");
    }

    close(pipefd[0]);
    close(epfd);
}

// ─── Test 4: poll_smoke — 2-fd poll where one fd closes ─────────────────
static void test_poll_two_fds_one_closes(void) {
    printf("Test 4: poll with 2 fds, one pipe closes (Nix multi-fd pattern)\n");
    int pipefd[2];
    TEST(pipe(pipefd) == 0, "pipe created");

    pid_t child = fork();
    TEST(child >= 0, "fork succeeded");

    if (child == 0) {
        close(pipefd[0]);
        (void)!write(pipefd[1], "!", 1);
        (void)!fsync(pipefd[1]);
        close(pipefd[1]);
        _exit(0);
    }

    close(pipefd[1]);

    // Also open another fd (e.g. a "control" fd via pipe-to-self)
    int ctrlfd[2];
    TEST(pipe(ctrlfd) == 0, "control pipe created");

    struct pollfd pfds[2] = {
        {.fd = pipefd[0], .events = POLLIN},
        {.fd = ctrlfd[0], .events = POLLIN},
    };

    // First poll: data on pipefd
    int ret = poll(pfds, 2, 200);
    TEST(ret >= 1, "multi-fd: poll got initial event(s)");
    TEST(pfds[0].revents & POLLIN, "multi-fd: pipe fd has POLLIN");

    // Drain
    char c;
    (void)!read(pipefd[0], &c, 1);

    // Wait for child exit
    waitpid(child, NULL, 0);

    // Poll again: pipe should report HUP
    pfds[0].revents = 0;
    pfds[1].revents = 0;
    ret = poll(pfds, 2, 200);
    TEST(ret >= 1, "multi-fd: poll detected pipe close");
    TEST((pfds[0].revents & (POLLIN | POLLHUP)) != 0,
         "multi-fd: pipe fd reports POLLIN or POLLHUP after close");
    fprintf(stderr, "  INFO: multi-fd pipe revents=0x%x ctrl revents=0x%x\n",
            pfds[0].revents, pfds[1].revents);

    close(pipefd[0]);
    close(ctrlfd[0]);
    close(ctrlfd[1]);
}

// ─── Test 5: poll close with wchan check ────────────────────────────────
static void test_poll_close_with_wchan(void) {
    printf("Test 5: wchan label during pipe-close poll\n");
    int pipefd[2];
    TEST(pipe(pipefd) == 0, "pipe created");

    pid_t child = fork();
    TEST(child >= 0, "fork succeeded");

    if (child == 0) {
        close(pipefd[0]);
        (void)!write(pipefd[1], ".", 1);
        (void)!fsync(pipefd[1]);
        close(pipefd[1]);
        _exit(0);
    }

    close(pipefd[1]);

    // Drain the data first
    char c;
    struct pollfd pfd = {.fd = pipefd[0], .events = POLLIN};
    poll(&pfd, 1, 200);
    (void)!read(pipefd[0], &c, 1);

    // Now poll should block (or not if pipe already closed)
    // Check wchan of ourselves — our child should show our wchan
    pid_t mypid = getpid();
    printf("  Parent PID=%d, child PID=%d\n", mypid, child);

    // Check if wchan is available for the child (zombie detection)
    char path[64];
    snprintf(path, sizeof(path), "/proc/%d/wchan", child);
    FILE *fp = fopen(path, "r");
    if (fp) {
        char wchan[128] = {0};
        if (fgets(wchan, sizeof(wchan), fp)) {
            printf("  Child wchan after exit: %s", wchan);
        }
        fclose(fp);
    }

    close(pipefd[0]);
    waitpid(child, NULL, 0);
}

// ─── Test 6: epoll LT + close without draining first ────────────────────
// Edge case: fd added to epoll, child writes and exits, parent hasn't
// drained yet.  epoll must deliver both IN (data) and HUP (close).
static void test_epoll_lt_close_with_data(void) {
    printf("Test 6: epoll LT delivers both data and close in one event\n");
    int pipefd[2];
    TEST(pipe(pipefd) == 0, "pipe created");

    int epfd = epoll_create1(0);
    TEST(epfd >= 0, "epoll_create1 succeeded");

    pid_t child = fork();
    TEST(child >= 0, "fork succeeded");

    if (child == 0) {
        close(pipefd[0]);
        (void)!write(pipefd[1], "data", 4);
        (void)!fsync(pipefd[1]);
        close(pipefd[1]);
        _exit(0);
    }

    close(pipefd[1]);

    struct epoll_event ev = {.events = EPOLLIN, .data.fd = pipefd[0]};
    TEST(epoll_ctl(epfd, EPOLL_CTL_ADD, pipefd[0], &ev) == 0,
         "epoll_ctl ADD succeeded");

    // Wait for child to exit
    waitpid(child, NULL, 0);

    // Now epoll should report events — both data and close
    struct epoll_event events[4];
    int nfds = epoll_wait(epfd, events, 4, 200);
    TEST(nfds == 1, "epoll_wait returned 1 event (data+close combined)");
    TEST(events[0].data.fd == pipefd[0],
         "event is for the pipe read end");
    fprintf(stderr,
            "  INFO: LT data+close events=0x%x (EPOLLIN=0x%x EPOLLHUP=0x%x)\n",
            events[0].events, EPOLLIN, EPOLLHUP);
    TEST((events[0].events & (EPOLLIN | EPOLLHUP)) != 0,
         "got EPOLLIN or EPOLLHUP for data+close");
    // EPOLLIN must be set because there is unconsumed data
    TEST(events[0].events & EPOLLIN,
         "EPOLLIN set (data present in buffer)");

    // Read data
    char buf[64] = {0};
    ssize_t n = read(pipefd[0], buf, sizeof(buf) - 1);
    TEST(n == 4, "read 4 bytes");
    TEST(strcmp(buf, "data") == 0, "correct data");

    // After draining, epoll should report HUP
    nfds = epoll_wait(epfd, events, 4, 200);
    TEST(nfds == 1, "after drain: epoll_wait returned close event");
    fprintf(stderr, "  INFO: after-drain close events=0x%x\n",
            events[0].events);
    TEST((events[0].events & (EPOLLIN | EPOLLHUP)) != 0,
         "after drain: got EPOLLIN or EPOLLHUP");

    close(pipefd[0]);
    close(epfd);
}

int main(void) {
    printf("=== pipe-poll-close regression ===\n");

    test_poll_pipe_close();
    test_epoll_pipe_close();
    test_epoll_in_only_detects_close();
    test_poll_two_fds_one_closes();
    test_poll_close_with_wchan();
    test_epoll_lt_close_with_data();

    printf("\n=== Results: %d pass, %d fail ===\n", tests_pass, tests_fail);
    if (tests_fail == 0) {
        printf("TEST PASSED\n");
    } else {
        printf("TEST FAILED\n");
    }
    return tests_fail > 0 ? 1 : 0;
}
