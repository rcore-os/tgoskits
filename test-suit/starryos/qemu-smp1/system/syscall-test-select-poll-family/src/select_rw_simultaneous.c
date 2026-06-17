#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_select_rw_simultaneous(void) {
    MODULE_START("select_rw_simultaneous");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    char c = 'A';
    write_exact(fds[1], &c, 1);

    fd_set rfds, wfds;
    FD_ZERO(&rfds);
    FD_ZERO(&wfds);
    FD_SET(fds[0], &rfds);
    FD_SET(fds[1], &wfds);
    struct timeval tv = {1, 0};
    int ret = raw_select(fds[1] + 1, &rfds, &wfds, NULL, &tv);
    CHECK(ret == 2, "read+write both ready returns 2");

    read_exact(fds[0], &c, 1);

    FD_ZERO(&wfds);
    FD_SET(fds[1], &wfds);
    tv.tv_sec = 1;
    tv.tv_usec = 0;
    ret = raw_select(fds[1] + 1, NULL, &wfds, NULL, &tv);
    CHECK(ret == 1, "write only returns 1");

    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);
    tv.tv_sec = 0;
    tv.tv_usec = 50000;
    ret = raw_select(fds[0] + 1, &rfds, NULL, NULL, &tv);
    CHECK(ret == 0, "read only empty pipe returns 0 timeout");

    close(fds[0]);
    close(fds[1]);

    MODULE_SUMMARY("select_rw_simultaneous");
    MODULE_RETURN();
}
