#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_select_fds_bits_matrix(void) {
    MODULE_START("select_fds_bits_matrix");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    int nfds = fds[1] + 1;
    struct timeval tv_short = {0, 50000};
    struct timeval tv;

    fd_set rfds, wfds, efds;

    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);
    tv = tv_short;
    CHECK_RET(raw_select(nfds, &rfds, NULL, NULL, &tv), 0, "empty pipe readfds only returns 0");

    FD_ZERO(&wfds);
    FD_SET(fds[1], &wfds);
    tv = tv_short;
    CHECK_RET(raw_select(nfds, NULL, &wfds, NULL, &tv), 1, "writefds only returns 1");

    FD_ZERO(&efds);
    FD_SET(fds[0], &efds);
    tv = tv_short;
    CHECK_RET(raw_select(nfds, NULL, NULL, &efds, &tv), 0, "exceptfds pipe read returns 0");

    tv = tv_short;
    CHECK_RET(raw_select(0, NULL, NULL, NULL, &tv), 0, "all NULL pure timeout returns 0");

    write_exact(fds[1], "A", 1);

    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);
    tv = tv_short;
    CHECK_RET(raw_select(nfds, &rfds, NULL, NULL, &tv), 1, "readfds with data returns 1");
    CHECK(FD_ISSET(fds[0], &rfds), "FD_ISSET read fd with data");

    FD_ZERO(&wfds);
    FD_SET(fds[1], &wfds);
    tv = tv_short;
    CHECK_RET(raw_select(nfds, NULL, &wfds, NULL, &tv), 1, "writefds with data returns 1");
    CHECK(FD_ISSET(fds[1], &wfds), "FD_ISSET write fd");

    FD_ZERO(&rfds);
    FD_ZERO(&wfds);
    FD_SET(fds[0], &rfds);
    FD_SET(fds[1], &wfds);
    tv = tv_short;
    CHECK_RET(raw_select(nfds, &rfds, &wfds, NULL, &tv), 2, "readfds+writefds with data returns 2");
    CHECK(FD_ISSET(fds[0], &rfds), "FD_ISSET read fd");
    CHECK(FD_ISSET(fds[1], &wfds), "FD_ISSET write fd");

    FD_ZERO(&rfds);
    FD_ZERO(&efds);
    FD_SET(fds[0], &rfds);
    FD_SET(fds[0], &efds);
    tv = tv_short;
    CHECK_RET(raw_select(nfds, &rfds, NULL, &efds, &tv), 1, "readfds+exceptfds with data returns 1");
    CHECK(FD_ISSET(fds[0], &rfds), "FD_ISSET read fd in readfds");
    CHECK(!FD_ISSET(fds[0], &efds), "FD_ISSET false in exceptfds");

    FD_ZERO(&rfds);
    FD_ZERO(&wfds);
    FD_ZERO(&efds);
    FD_SET(fds[0], &rfds);
    FD_SET(fds[1], &wfds);
    FD_SET(fds[0], &efds);
    tv = tv_short;
    long ret = raw_select(nfds, &rfds, &wfds, &efds, &tv);
    CHECK(ret >= 2, "readfds+writefds+exceptfds with data returns >=2");
    CHECK(FD_ISSET(fds[0], &rfds), "FD_ISSET read fd in readfds");
    CHECK(FD_ISSET(fds[1], &wfds), "FD_ISSET write fd in writefds");

    FD_ZERO(&wfds);
    FD_ZERO(&efds);
    FD_SET(fds[1], &wfds);
    FD_SET(fds[0], &efds);
    tv = tv_short;
    CHECK_RET(raw_select(nfds, NULL, &wfds, &efds, &tv), 1, "writefds+exceptfds returns 1");
    CHECK(FD_ISSET(fds[1], &wfds), "FD_ISSET write fd in writefds");

    close(fds[0]);
    close(fds[1]);

    MODULE_SUMMARY("select_fds_bits_matrix");
    MODULE_RETURN();
}
