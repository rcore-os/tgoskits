#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_select_devnull(void) {
    MODULE_START("select_devnull");

    int rfd = open("/dev/null", O_RDONLY);
    CHECK(rfd >= 0, "open /dev/null O_RDONLY");

    fd_set rfds;
    FD_ZERO(&rfds);
    FD_SET(rfd, &rfds);
    struct timeval tv = {1, 0};
    int ret = raw_select(rfd + 1, &rfds, NULL, NULL, &tv);
    CHECK(ret == 1, "select /dev/null readfds returns 1");

    close(rfd);

    int wfd = open("/dev/null", O_WRONLY);
    CHECK(wfd >= 0, "open /dev/null O_WRONLY");

    fd_set wfds;
    FD_ZERO(&wfds);
    FD_SET(wfd, &wfds);
    tv.tv_sec = 1;
    tv.tv_usec = 0;
    ret = raw_select(wfd + 1, NULL, &wfds, NULL, &tv);
    CHECK(ret == 1, "select /dev/null writefds returns 1");

    close(wfd);

    MODULE_SUMMARY("select_devnull");
    MODULE_RETURN();
}
