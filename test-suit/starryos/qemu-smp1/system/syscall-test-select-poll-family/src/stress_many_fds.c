#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_stress_many_fds(void) {
    MODULE_START("stress_many_fds");

    {
        int p[16][2];
        for (int i = 0; i < 16; i++) {
            CHECK_RET(create_pipe(p[i]), 0, "pipe created");
        }

        for (int i = 0; i < 8; i++) {
            write_exact(p[i * 2][1], "X", 1);
        }

        fd_set rfds;
        FD_ZERO(&rfds);
        int nfds = 0;
        for (int i = 0; i < 16; i++) {
            FD_SET(p[i][0], &rfds);
            if (p[i][0] + 1 > nfds) nfds = p[i][0] + 1;
        }
        struct timeval tv = {1, 0};
        long ret = raw_select(nfds, &rfds, NULL, NULL, &tv);
        CHECK(ret == 8, "select 16 pipes 8 written returns 8");

        int count = 0;
        for (int i = 0; i < 16; i++) {
            if (FD_ISSET(p[i][0], &rfds)) count++;
        }
        CHECK(count == 8, "select correct number of FD_ISSET");

        struct pollfd pfds[16];
        for (int i = 0; i < 16; i++) {
            pfds[i].fd = p[i][0];
            pfds[i].events = POLLIN;
            pfds[i].revents = 0;
        }
        ret = raw_poll(pfds, 16, 1000);
        CHECK(ret == 8, "poll 16 pipes 8 written returns 8");

        count = 0;
        int pollin_count = 0;
        for (int i = 0; i < 16; i++) {
            if (pfds[i].revents & POLLIN) {
                pollin_count++;
            }
            count++;
        }
        CHECK(pollin_count == 8, "poll correct revents POLLIN count");

        for (int i = 0; i < 16; i++) {
            close(p[i][0]);
            close(p[i][1]);
        }
    }

    {
        int p[32][2];
        for (int i = 0; i < 32; i++) {
            CHECK_RET(create_pipe(p[i]), 0, "pipe32 created");
        }

        for (int i = 0; i < 16; i++) {
            write_exact(p[i][1], "Y", 1);
        }

        fd_set rfds;
        FD_ZERO(&rfds);
        int nfds = 0;
        for (int i = 0; i < 32; i++) {
            FD_SET(p[i][0], &rfds);
            if (p[i][0] + 1 > nfds) nfds = p[i][0] + 1;
        }
        struct timeval tv = {1, 0};
        long ret = raw_select(nfds, &rfds, NULL, NULL, &tv);
        CHECK(ret == 16, "select 32 pipes 16 written returns 16");

        struct pollfd pfds[32];
        for (int i = 0; i < 32; i++) {
            pfds[i].fd = p[i][0];
            pfds[i].events = POLLIN;
            pfds[i].revents = 0;
        }
        ret = raw_poll(pfds, 32, 1000);
        CHECK(ret == 16, "poll 32 pipes 16 written returns 16");

        for (int i = 0; i < 32; i++) {
            close(p[i][0]);
            close(p[i][1]);
        }
    }

    MODULE_SUMMARY("stress_many_fds");
    MODULE_RETURN();
}
