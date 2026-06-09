#include "test_framework.h"
#include <sys/select.h>
#include <sys/time.h>
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

// Helper to invoke raw select if available
static int raw_select(int nfds, fd_set *readfds, fd_set *writefds, fd_set *exceptfds, struct timeval *timeout) {
#ifdef SYS_select
    return syscall(SYS_select, nfds, readfds, writefds, exceptfds, timeout);
#else
    return select(nfds, readfds, writefds, exceptfds, timeout);
#endif
}

// Helper to invoke raw poll if available
static int raw_poll(struct pollfd *fds, nfds_t nfds, int timeout) {
#ifdef SYS_poll
    return syscall(SYS_poll, fds, nfds, timeout);
#else
    return poll(fds, nfds, timeout);
#endif
}

static void test_select_timeout(void) {
    struct timeval tv;
    tv.tv_sec = 0;
    tv.tv_usec = 10000; // 10ms

    fd_set readfds;
    FD_ZERO(&readfds);

    int ret = raw_select(0, &readfds, NULL, NULL, &tv);
    CHECK(ret == 0, "select empty fds with timeout returns 0");
}

static void test_select_pipe(void) {
    int fds[2];
    CHECK_RET(pipe(fds), 0, "pipe created");

    fd_set readfds;
    FD_ZERO(&readfds);
    FD_SET(fds[0], &readfds);

    struct timeval tv;
    tv.tv_sec = 0;
    tv.tv_usec = 10000; // 10ms

    // empty pipe, should timeout
    int ret = raw_select(fds[0] + 1, &readfds, NULL, NULL, &tv);
    CHECK(ret == 0, "select empty pipe returns 0 (timeout)");

    // write data
    CHECK_RET(write(fds[1], "A", 1), 1, "write 1 byte to pipe");

    FD_ZERO(&readfds);
    FD_SET(fds[0], &readfds);
    tv.tv_sec = 0;
    tv.tv_usec = 10000; // 10ms

    // now pipe has data, should return 1 readable fd
    ret = raw_select(fds[0] + 1, &readfds, NULL, NULL, &tv);
    CHECK(ret == 1, "select pipe with data returns 1");
    CHECK(FD_ISSET(fds[0], &readfds), "FD_ISSET is true for read fd");

    close(fds[0]);
    close(fds[1]);
}

static void test_poll_timeout(void) {
    int ret = raw_poll(NULL, 0, 10); // 10ms
    CHECK(ret == 0, "poll empty fds with timeout returns 0");
}

static void test_poll_pipe(void) {
    int fds[2];
    CHECK_RET(pipe(fds), 0, "pipe created");

    struct pollfd pfd;
    pfd.fd = fds[0];
    pfd.events = POLLIN;

    // empty pipe, should timeout
    int ret = raw_poll(&pfd, 1, 10); // 10ms
    CHECK(ret == 0, "poll empty pipe returns 0 (timeout)");

    // write data
    CHECK_RET(write(fds[1], "A", 1), 1, "write 1 byte to pipe");

    // now pipe has data, should return 1 readable fd
    ret = raw_poll(&pfd, 1, 10); // 10ms
    CHECK(ret == 1, "poll pipe with data returns 1");
    CHECK(pfd.revents & POLLIN, "POLLIN is set in revents");

    close(fds[0]);
    close(fds[1]);
}

int main(void) {
    TEST_START("select/poll semantics");

    test_select_timeout();
    test_select_pipe();

    test_poll_timeout();
    test_poll_pipe();

    TEST_DONE();
    return 0;
}
