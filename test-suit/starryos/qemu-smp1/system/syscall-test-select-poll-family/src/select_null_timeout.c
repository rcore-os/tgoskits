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
    int sync_fds[2];
    CHECK_RET(create_pipe(sync_fds), 0, "sync pipe created");

    pid_t pid = fork();
    CHECK(pid >= 0, "fork succeeded");

    if (pid == 0) {
        close(fds[0]);
        close(sync_fds[1]);

        usleep(50000);
        char c = 'X';
        if (write_exact(fds[1], &c, 1) != 0) {
            _exit(1);
        }

        char release;
        if (read_exact(sync_fds[0], &release, 1) != 0) {
            _exit(2);
        }

        close(fds[1]);
        close(sync_fds[0]);
        _exit(0);
    }

    close(sync_fds[0]);

    fd_set rfds;
    int ret;
    do {
        FD_ZERO(&rfds);
        FD_SET(fds[0], &rfds);
        errno = 0;
        ret = raw_select(fds[0] + 1, &rfds, NULL, NULL, NULL);
    } while (ret == -1 && errno == EINTR);

    CHECK(ret == 1, "select(timeout=NULL) returns 1 after child writes");
    CHECK(FD_ISSET(fds[0], &rfds), "FD_ISSET true for read fd after child writes");

    char release = 'R';
    CHECK_RET(write_exact(sync_fds[1], &release, 1), 0,
              "release child after select observes pipe");

    int status;
    waitpid(pid, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "child exits cleanly");

    close(fds[0]);
    close(fds[1]);
    close(sync_fds[1]);

    MODULE_SUMMARY("select_null_timeout");
    MODULE_RETURN();
}
