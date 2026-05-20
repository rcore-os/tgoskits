#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_select_must_reinit(void) {
    MODULE_START("select_must_reinit");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    char c = 'A';
    write_exact(fds[1], &c, 1);

    fd_set rfds;
    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);
    struct timeval tv = {1, 0};
    int ret = raw_select(fds[0] + 1, &rfds, NULL, NULL, &tv);
    CHECK(ret == 1, "first select returns 1");

    read_exact(fds[0], &c, 1);

    tv.tv_sec = 0;
    tv.tv_usec = 50000;
    ret = raw_select(fds[0] + 1, &rfds, NULL, NULL, &tv);
    CHECK(ret == 0, "select without reinit returns 0 fd_set was modified");

    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);
    tv.tv_sec = 0;
    tv.tv_usec = 50000;
    ret = raw_select(fds[0] + 1, &rfds, NULL, NULL, &tv);
    CHECK(ret == 0, "select with reinit returns 0 timeout");

    close(fds[0]);
    close(fds[1]);

    MODULE_SUMMARY("select_must_reinit");
    MODULE_RETURN();
}
