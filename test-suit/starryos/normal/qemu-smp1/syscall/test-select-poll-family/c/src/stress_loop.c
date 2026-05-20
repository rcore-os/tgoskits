#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_stress_loop(void) {
    MODULE_START("stress_loop");

    {
        int fds[2];
        CHECK_RET(create_pipe(fds), 0, "pipe created for select loop");

        int pass_count = 0;
        int err_count = 0;
        for (int i = 0; i < 100; i++) {
            char w = (char)(i & 0xFF);
            write_exact(fds[1], &w, 1);

            fd_set rfds;
            FD_ZERO(&rfds);
            FD_SET(fds[0], &rfds);
            struct timeval tv = {1, 0};
            long ret = raw_select(fds[0] + 1, &rfds, NULL, NULL, &tv);

            if (ret != 1 || !FD_ISSET(fds[0], &rfds)) {
                err_count++;
                continue;
            }

            char r = 0;
            if (read_exact(fds[0], &r, 1) != 0 || r != w) {
                err_count++;
                continue;
            }
            pass_count++;
        }

        CHECK(err_count == 0, "select 100 loop no errors");
        CHECK(pass_count == 100, "select 100 loop all passed");

        close(fds[0]);
        close(fds[1]);
    }

    {
        int fds[2];
        CHECK_RET(create_pipe(fds), 0, "pipe created for poll loop");

        int pass_count = 0;
        int err_count = 0;
        for (int i = 0; i < 100; i++) {
            char w = (char)(i & 0xFF);
            write_exact(fds[1], &w, 1);

            struct pollfd pfd = { .fd = fds[0], .events = POLLIN, .revents = 0 };
            long ret = raw_poll(&pfd, 1, 1000);

            if (ret != 1 || !(pfd.revents & POLLIN)) {
                err_count++;
                continue;
            }

            char r = 0;
            if (read_exact(fds[0], &r, 1) != 0 || r != w) {
                err_count++;
                continue;
            }
            pass_count++;
        }

        CHECK(err_count == 0, "poll 100 loop no errors");
        CHECK(pass_count == 100, "poll 100 loop all passed");

        close(fds[0]);
        close(fds[1]);
    }

    MODULE_SUMMARY("stress_loop");
    MODULE_RETURN();
}
