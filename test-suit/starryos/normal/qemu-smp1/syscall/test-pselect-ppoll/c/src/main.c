#include "test_framework.h"
#include <sys/select.h>
#include <sys/time.h>
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <signal.h>
#include <sys/syscall.h>

#define KERNEL_SIGSET_SIZE 8

// Helper to invoke raw pselect6 if available
static int raw_pselect6(int nfds, fd_set *readfds, fd_set *writefds, fd_set *exceptfds, struct timespec *timeout, const sigset_t *sigmask) {
#ifdef SYS_pselect6
    // pselect6 expects a pointer to a struct { sigset_t *ss; size_t ss_len; }
    // libc wrapper does this mapping. For raw syscall, we construct it:
    struct {
        const sigset_t *ss;
        size_t ss_len;
    } data = { sigmask, KERNEL_SIGSET_SIZE };
    return syscall(SYS_pselect6, nfds, readfds, writefds, exceptfds, timeout, &data);
#else
    return pselect(nfds, readfds, writefds, exceptfds, timeout, sigmask);
#endif
}

// Helper to invoke raw ppoll if available
static int raw_ppoll(struct pollfd *fds, nfds_t nfds, const struct timespec *tmo_p, const sigset_t *sigmask) {
#ifdef SYS_ppoll
    return syscall(SYS_ppoll, fds, nfds, tmo_p, sigmask, KERNEL_SIGSET_SIZE);
#else
    return ppoll(fds, nfds, tmo_p, sigmask);
#endif
}

static void test_pselect_timeout(void) {
    struct timespec ts;
    ts.tv_sec = 0;
    ts.tv_nsec = 10000000; // 10ms

    fd_set readfds;
    FD_ZERO(&readfds);

    sigset_t sigmask;
    sigemptyset(&sigmask);

    int ret = raw_pselect6(0, &readfds, NULL, NULL, &ts, &sigmask);
    CHECK(ret == 0, "pselect empty fds with timeout returns 0");
}

static void test_pselect_pipe(void) {
    int fds[2];
    CHECK_RET(pipe(fds), 0, "pipe created");

    fd_set readfds;
    FD_ZERO(&readfds);
    FD_SET(fds[0], &readfds);

    struct timespec ts;
    ts.tv_sec = 0;
    ts.tv_nsec = 10000000; // 10ms

    sigset_t sigmask;
    sigemptyset(&sigmask);

    // empty pipe, should timeout
    int ret = raw_pselect6(fds[0] + 1, &readfds, NULL, NULL, &ts, &sigmask);
    CHECK(ret == 0, "pselect empty pipe returns 0 (timeout)");

    // write data
    CHECK_RET(write(fds[1], "A", 1), 1, "write 1 byte to pipe");

    FD_ZERO(&readfds);
    FD_SET(fds[0], &readfds);
    ts.tv_sec = 0;
    ts.tv_nsec = 10000000; // 10ms

    // now pipe has data, should return 1 readable fd
    ret = raw_pselect6(fds[0] + 1, &readfds, NULL, NULL, &ts, &sigmask);
    CHECK(ret == 1, "pselect pipe with data returns 1");
    CHECK(FD_ISSET(fds[0], &readfds), "FD_ISSET is true for read fd");

    close(fds[0]);
    close(fds[1]);
}

static void test_ppoll_timeout(void) {
    struct timespec ts;
    ts.tv_sec = 0;
    ts.tv_nsec = 10000000; // 10ms

    sigset_t sigmask;
    sigemptyset(&sigmask);

    int ret = raw_ppoll(NULL, 0, &ts, &sigmask);
    CHECK(ret == 0, "ppoll empty fds with timeout returns 0");
}

static void test_ppoll_pipe(void) {
    int fds[2];
    CHECK_RET(pipe(fds), 0, "pipe created");

    struct pollfd pfd;
    pfd.fd = fds[0];
    pfd.events = POLLIN;

    struct timespec ts;
    ts.tv_sec = 0;
    ts.tv_nsec = 10000000; // 10ms

    sigset_t sigmask;
    sigemptyset(&sigmask);

    // empty pipe, should timeout
    int ret = raw_ppoll(&pfd, 1, &ts, &sigmask);
    CHECK(ret == 0, "ppoll empty pipe returns 0 (timeout)");

    // write data
    CHECK_RET(write(fds[1], "A", 1), 1, "write 1 byte to pipe");

    // now pipe has data, should return 1 readable fd
    ret = raw_ppoll(&pfd, 1, &ts, &sigmask);
    CHECK(ret == 1, "ppoll pipe with data returns 1");
    CHECK(pfd.revents & POLLIN, "POLLIN is set in revents");

    close(fds[0]);
    close(fds[1]);
}

int main(void) {
    TEST_START("pselect/ppoll semantics");

    test_pselect_timeout();
    test_pselect_pipe();

    test_ppoll_timeout();
    test_ppoll_pipe();

    TEST_DONE();
    return 0;
}
