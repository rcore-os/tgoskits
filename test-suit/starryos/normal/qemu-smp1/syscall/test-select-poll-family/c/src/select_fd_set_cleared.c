#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_select_fd_set_cleared(void) {
    MODULE_START("select_fd_set_cleared");

    int p1[2], p2[2];
    CHECK_RET(create_pipe(p1), 0, "pipe1 created");
    CHECK_RET(create_pipe(p2), 0, "pipe2 created");

    char c = 'A';
    write_exact(p1[1], &c, 1);

    fd_set rfds;
    FD_ZERO(&rfds);
    FD_SET(p1[0], &rfds);
    FD_SET(p2[0], &rfds);

    int nfds = p1[0] > p2[0] ? p1[0] : p2[0];
    nfds++;

    struct timeval tv = {1, 0};
    int ret = raw_select(nfds, &rfds, NULL, NULL, &tv);
    CHECK(ret == 1, "select returns 1 only pipe1 has data");
    CHECK(FD_ISSET(p1[0], &rfds), "pipe1 FD_ISSET true");
    CHECK(!FD_ISSET(p2[0], &rfds), "pipe2 FD_ISSET false cleared");

    close(p1[0]); close(p1[1]);
    close(p2[0]); close(p2[1]);

    MODULE_SUMMARY("select_fd_set_cleared");
    MODULE_RETURN();
}
