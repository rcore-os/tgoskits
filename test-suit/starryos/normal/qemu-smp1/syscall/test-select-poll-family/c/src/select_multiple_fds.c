#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_select_multiple_fds(void) {
    MODULE_START("select_multiple_fds");

    int p1[2], p2[2], p3[2];
    CHECK_RET(create_pipe(p1), 0, "pipe1 created");
    CHECK_RET(create_pipe(p2), 0, "pipe2 created");
    CHECK_RET(create_pipe(p3), 0, "pipe3 created");

    char c = 'A';
    write_exact(p2[1], &c, 1);

    fd_set rfds;
    FD_ZERO(&rfds);
    FD_SET(p1[0], &rfds);
    FD_SET(p2[0], &rfds);
    FD_SET(p3[0], &rfds);

    int nfds = p1[0];
    if (p2[0] > nfds) nfds = p2[0];
    if (p3[0] > nfds) nfds = p3[0];
    nfds++;

    struct timeval tv = {1, 0};
    int ret = raw_select(nfds, &rfds, NULL, NULL, &tv);
    CHECK(ret == 1, "select 3 pipes only pipe2 written returns 1");
    CHECK(!FD_ISSET(p1[0], &rfds), "pipe1 not set");
    CHECK(FD_ISSET(p2[0], &rfds), "pipe2 set");
    CHECK(!FD_ISSET(p3[0], &rfds), "pipe3 not set");

    write_exact(p1[1], &c, 1);
    write_exact(p3[1], &c, 1);

    FD_ZERO(&rfds);
    FD_SET(p1[0], &rfds);
    FD_SET(p2[0], &rfds);
    FD_SET(p3[0], &rfds);
    tv.tv_sec = 1;
    tv.tv_usec = 0;
    ret = raw_select(nfds, &rfds, NULL, NULL, &tv);
    CHECK(ret == 3, "select all 3 pipes written returns 3");
    CHECK(FD_ISSET(p1[0], &rfds), "pipe1 set");
    CHECK(FD_ISSET(p2[0], &rfds), "pipe2 set");
    CHECK(FD_ISSET(p3[0], &rfds), "pipe3 set");

    close(p1[0]); close(p1[1]);
    close(p2[0]); close(p2[1]);
    close(p3[0]); close(p3[1]);

    MODULE_SUMMARY("select_multiple_fds");
    MODULE_RETURN();
}
