#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_select_read_pipe(void) {
    MODULE_START("select_read_pipe");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    char buf[256];

    char c = 'A';
    write_exact(fds[1], &c, 1);
    fd_set rfds;
    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);
    struct timeval tv = {1, 0};
    int ret = raw_select(fds[0] + 1, &rfds, NULL, NULL, &tv);
    CHECK(ret == 1, "select returns 1 after 1 byte written");
    CHECK(FD_ISSET(fds[0], &rfds), "FD_ISSET true after 1 byte");
    read_exact(fds[0], &c, 1);

    char data5[5] = {1, 2, 3, 4, 5};
    write_exact(fds[1], data5, 5);
    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);
    tv.tv_sec = 1;
    tv.tv_usec = 0;
    ret = raw_select(fds[0] + 1, &rfds, NULL, NULL, &tv);
    CHECK(ret == 1, "select returns 1 after 5 bytes written");
    read_exact(fds[0], buf, 5);

    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);
    tv.tv_sec = 0;
    tv.tv_usec = 50000;
    ret = raw_select(fds[0] + 1, &rfds, NULL, NULL, &tv);
    CHECK(ret == 0, "select returns 0 after all data read");

    char big[256];
    memset(big, 'B', sizeof(big));
    write_exact(fds[1], big, 256);
    char one;
    read_exact(fds[0], &one, 1);
    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);
    tv.tv_sec = 1;
    tv.tv_usec = 0;
    ret = raw_select(fds[0] + 1, &rfds, NULL, NULL, &tv);
    CHECK(ret == 1, "select returns 1 with 255 bytes remaining");

    close(fds[0]);
    close(fds[1]);

    MODULE_SUMMARY("select_read_pipe");
    MODULE_RETURN();
}
