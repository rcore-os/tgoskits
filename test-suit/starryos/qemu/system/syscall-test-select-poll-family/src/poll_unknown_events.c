#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <poll.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#define UNKNOWN_POLL_EVENT ((short)0x0800)
#define KERNEL_SIGSET_SIZE (sizeof(unsigned long))

int run_poll_unknown_events(void) {
    MODULE_START("poll_unknown_events");
    long ret;

#ifdef SYS_poll
    int poll_fds[2];
    CHECK_RET(create_pipe(poll_fds), 0, "poll pipe created");

    struct pollfd poll_fd = {
        .fd = poll_fds[0],
        .events = UNKNOWN_POLL_EVENT,
        .revents = 0,
    };
    errno = 0;
    ret = syscall(SYS_poll, &poll_fd, 1, 0);
    CHECK(ret == 0 && errno == 0,
          "poll ignores an unknown event while the writer is open");
    CHECK(poll_fd.revents == 0,
          "poll does not echo an unknown event while the writer is open");

    CHECK_RET(close(poll_fds[1]), 0, "poll writer closed");
    poll_fd.revents = 0;
    errno = 0;
    ret = syscall(SYS_poll, &poll_fd, 1, 0);
    CHECK(ret == 1 && errno == 0,
          "poll reports one ready fd instead of EINVAL after writer close");
    CHECK((poll_fd.revents & POLLHUP) != 0,
          "poll reports POLLHUP even when only an unknown event was requested");
    CHECK((poll_fd.revents & UNKNOWN_POLL_EVENT) == 0,
          "poll does not echo the unknown event after writer close");

    CHECK_RET(close(poll_fds[0]), 0, "poll reader closed");
#else
    printf("  SKIP | raw SYS_poll is unavailable on this architecture\n");
#endif

    int ppoll_fds[2];
    CHECK_RET(create_pipe(ppoll_fds), 0, "ppoll pipe created");

    struct pollfd ppoll_fd = {
        .fd = ppoll_fds[0],
        .events = UNKNOWN_POLL_EVENT,
        .revents = 0,
    };
    struct timespec timeout = { .tv_sec = 0, .tv_nsec = 0 };
    errno = 0;
    ret = syscall(SYS_ppoll, &ppoll_fd, 1, &timeout, NULL,
                  KERNEL_SIGSET_SIZE);
    CHECK(ret == 0 && errno == 0,
          "ppoll ignores an unknown event while the writer is open");
    CHECK(ppoll_fd.revents == 0,
          "ppoll does not echo an unknown event while the writer is open");

    CHECK_RET(close(ppoll_fds[1]), 0, "ppoll writer closed");
    ppoll_fd.revents = 0;
    timeout.tv_sec = 0;
    timeout.tv_nsec = 0;
    errno = 0;
    ret = syscall(SYS_ppoll, &ppoll_fd, 1, &timeout, NULL,
                  KERNEL_SIGSET_SIZE);
    CHECK(ret == 1 && errno == 0,
          "ppoll reports one ready fd instead of EINVAL after writer close");
    CHECK((ppoll_fd.revents & POLLHUP) != 0,
          "ppoll reports POLLHUP even when only an unknown event was requested");
    CHECK((ppoll_fd.revents & UNKNOWN_POLL_EVENT) == 0,
          "ppoll does not echo the unknown event after writer close");

    CHECK_RET(close(ppoll_fds[0]), 0, "ppoll reader closed");

    MODULE_SUMMARY("poll_unknown_events");
    MODULE_RETURN();
}
