#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_poll_multiple_fds(void) {
    MODULE_START("poll_multiple_fds");

    int p1[2], p2[2], p3[2];
    CHECK_RET(create_pipe(p1), 0, "pipe1 created");
    CHECK_RET(create_pipe(p2), 0, "pipe2 created");
    CHECK_RET(create_pipe(p3), 0, "pipe3 created");

    write_exact(p2[1], "B", 1);

    struct pollfd pfds[3];
    pfds[0].fd = p1[0]; pfds[0].events = POLLIN; pfds[0].revents = 0;
    pfds[1].fd = p2[0]; pfds[1].events = POLLIN; pfds[1].revents = 0;
    pfds[2].fd = p3[0]; pfds[2].events = POLLIN; pfds[2].revents = 0;

    CHECK_RET(raw_poll(pfds, 3, 100), 1, "only pipe2 written returns 1");
    CHECK(!(pfds[0].revents & POLLIN), "pipe1 revents no POLLIN");
    CHECK(pfds[1].revents & POLLIN, "pipe2 revents has POLLIN");
    CHECK(!(pfds[2].revents & POLLIN), "pipe3 revents no POLLIN");

    write_exact(p1[1], "A", 1);
    write_exact(p3[1], "C", 1);

    pfds[0].revents = 0;
    pfds[1].revents = 0;
    pfds[2].revents = 0;
    CHECK_RET(raw_poll(pfds, 3, 100), 3, "all three pipes have data returns 3");
    CHECK(pfds[0].revents & POLLIN, "pipe1 revents has POLLIN");
    CHECK(pfds[1].revents & POLLIN, "pipe2 revents has POLLIN");
    CHECK(pfds[2].revents & POLLIN, "pipe3 revents has POLLIN");

    close(p1[0]); close(p1[1]);
    close(p2[0]); close(p2[1]);
    close(p3[0]); close(p3[1]);

    MODULE_SUMMARY("poll_multiple_fds");
    MODULE_RETURN();
}
