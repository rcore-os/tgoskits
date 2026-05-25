#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>
#include <sys/wait.h>

int run_select_null_timeout(void) {
    MODULE_START("select_null_timeout");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    pid_t pid = fork();
    CHECK(pid >= 0, "fork succeeded");

    if (pid == 0) {
        usleep(50000);
        char c = 'X';
        write_exact(fds[1], &c, 1);
        close(fds[0]);
        close(fds[1]);
        _exit(0);
    }

    fd_set rfds;
    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);
    int ret = raw_select(fds[0] + 1, &rfds, NULL, NULL, NULL);
    CHECK(ret == 1, "select(timeout=NULL) returns 1 after child writes");
    CHECK(FD_ISSET(fds[0], &rfds), "FD_ISSET true for read fd after child writes");

    int status;
    waitpid(pid, &status, 0);

    close(fds[0]);
    close(fds[1]);

    MODULE_SUMMARY("select_null_timeout");
    MODULE_RETURN();
}
