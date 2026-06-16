#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_select_zero_timeout(void) {
    MODULE_START("select_zero_timeout");

    fd_set rfds;
    FD_ZERO(&rfds);
    struct timeval tv = {0, 0};
    int ret = raw_select(0, &rfds, NULL, NULL, &tv);
    CHECK(ret == 0, "select zero timeout empty fds returns 0");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);
    tv.tv_sec = 0;
    tv.tv_usec = 0;
    ret = raw_select(fds[0] + 1, &rfds, NULL, NULL, &tv);
    CHECK(ret == 0, "select zero timeout empty pipe returns 0");

    char c = 'A';
    write_exact(fds[1], &c, 1);

    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);
    tv.tv_sec = 0;
    tv.tv_usec = 0;
    ret = raw_select(fds[0] + 1, &rfds, NULL, NULL, &tv);
    CHECK(ret == 1, "select zero timeout pipe with data returns 1");
    CHECK(FD_ISSET(fds[0], &rfds), "FD_ISSET true for read fd with data");

    close(fds[0]);
    close(fds[1]);

    MODULE_SUMMARY("select_zero_timeout");
    MODULE_RETURN();
}
