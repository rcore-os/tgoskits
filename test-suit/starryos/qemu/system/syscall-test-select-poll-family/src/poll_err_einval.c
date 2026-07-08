#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_poll_err_einval(void) {
    MODULE_START("poll_err_einval");

    struct pollfd pfds[1];
    pfds[0].fd = 0;
    pfds[0].events = POLLIN;
    pfds[0].revents = 0;

    errno = 0;
    long ret = raw_poll(pfds, 999999, 0);
    if (ret == -1 && (errno == EINVAL || errno == ENOMEM || errno == EFAULT)) {
        __pass++;
    } else {
        printf("  FAIL | %s:%d | huge nfds expected EINVAL/ENOMEM/EFAULT got ret=%ld errno=%d (%s)\n",
               __FILE__, __LINE__, ret, errno, strerror(errno));
        __fail++;
    }

    ret = raw_poll(NULL, 0, 10);
    CHECK(ret == 0, "poll nfds=0 with timeout returns 0");

    MODULE_SUMMARY("poll_err_einval");
    MODULE_RETURN();
}
