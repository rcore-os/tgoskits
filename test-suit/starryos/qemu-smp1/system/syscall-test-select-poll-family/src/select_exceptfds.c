#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_select_exceptfds(void) {
    MODULE_START("select_exceptfds");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    fd_set efds;
    FD_ZERO(&efds);
    FD_SET(fds[0], &efds);
    struct timeval tv = {0, 50000};
    int ret = raw_select(fds[0] + 1, NULL, NULL, &efds, &tv);
    CHECK(ret == 0, "select exceptfds on pipe timeout returns 0");

    char c = 'A';
    write_exact(fds[1], &c, 1);

    fd_set rfds;
    FD_ZERO(&rfds);
    FD_ZERO(&efds);
    FD_SET(fds[0], &rfds);
    FD_SET(fds[0], &efds);
    tv.tv_sec = 1;
    tv.tv_usec = 0;
    ret = raw_select(fds[0] + 1, &rfds, NULL, &efds, &tv);
    CHECK(ret == 1, "select readfds+exceptfds with data returns 1");
    CHECK(FD_ISSET(fds[0], &rfds), "FD_ISSET true in readfds");
    CHECK(!FD_ISSET(fds[0], &efds), "FD_ISSET false in exceptfds");

    close(fds[0]);
    close(fds[1]);

    MODULE_SUMMARY("select_exceptfds");
    MODULE_RETURN();
}
