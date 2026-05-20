#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_select_nfds_semantics(void) {
    MODULE_START("select_nfds_semantics");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    char c = 'A';
    write_exact(fds[1], &c, 1);

    fd_set rfds;
    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);
    struct timeval tv = {0, 50000};
    int ret = raw_select(0, &rfds, NULL, NULL, &tv);
    CHECK(ret == 0, "select nfds=0 ignores fd returns 0");

    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);
    tv.tv_sec = 0;
    tv.tv_usec = 50000;
    ret = raw_select(fds[0], &rfds, NULL, NULL, &tv);
    CHECK(ret == 0, "select nfds=pipe_read_fd ignores fd returns 0");

    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);
    tv.tv_sec = 1;
    tv.tv_usec = 0;
    ret = raw_select(fds[0] + 1, &rfds, NULL, NULL, &tv);
    CHECK(ret == 1, "select nfds=pipe_read_fd+1 detects fd returns 1");

    close(fds[0]);
    close(fds[1]);

    MODULE_SUMMARY("select_nfds_semantics");
    MODULE_RETURN();
}
