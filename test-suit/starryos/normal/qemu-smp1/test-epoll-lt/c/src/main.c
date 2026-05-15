/*
 * Regression test for epoll level-triggered duplicate-event bug.
 *
 * Before the txlist fix, Epoll::poll_events used a pop_front + push_back
 * pattern on the same ready_queue inside a single epoll_wait call. A
 * single ready fd in LT mode was therefore re-fed into the loop and
 * filled the caller's output array with maxevents copies of itself.
 * epoll_wait(maxevents=4) returned n=4 with all entries pointing at the
 * same fd, instead of n=1.
 *
 * After the fix (mem::take + keep queue + post-loop splice, mirroring
 * Linux ep_send_events txlist), each interest is visited at most once
 * per epoll_wait, so a single ready fd yields exactly one event entry.
 *
 * The test sets up an AF_UNIX listen socket + epoll, forks a child that
 * connects, and asserts that epoll_wait(maxevents=4) on the parent
 * returns exactly n=1 with EPOLLIN on the listen fd. Same shape for the
 * data-arrival event on the accepted fd. Pre-fix: n=4 -> FAIL.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

static int pass = 0, fail = 0;
#define CHECK(cond, name) do { \
    if (cond) { printf("PASS: epoll_lt::%s\n", name); pass++; } \
    else      { printf("FAIL: epoll_lt::%s (errno=%d)\n", name, errno); fail++; } \
} while (0)

int main(void) {
    const char *path = "/tmp/epoll_lt_test.sock";
    unlink(path);

    int srv = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    CHECK(srv >= 0, "socket");

    struct sockaddr_un sa;
    memset(&sa, 0, sizeof(sa));
    sa.sun_family = AF_UNIX;
    strncpy(sa.sun_path, path, sizeof(sa.sun_path) - 1);
    CHECK(bind(srv, (struct sockaddr *)&sa, sizeof(sa)) == 0, "bind");
    CHECK(listen(srv, 4) == 0, "listen");

    int ep = epoll_create1(EPOLL_CLOEXEC);
    CHECK(ep >= 0, "epoll_create1");

    struct epoll_event ev;
    memset(&ev, 0, sizeof(ev));
    ev.events = EPOLLIN;
    ev.data.fd = srv;
    CHECK(epoll_ctl(ep, EPOLL_CTL_ADD, srv, &ev) == 0, "epoll_add_listen");

    pid_t pid = fork();
    CHECK(pid >= 0, "fork");
    if (pid == 0) {
        /* child: let parent reach epoll_wait, then connect + write */
        sleep(1);
        int cli = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
        if (cli < 0) { perror("child socket"); _exit(1); }
        if (connect(cli, (struct sockaddr *)&sa, sizeof(sa)) != 0) {
            perror("child connect"); _exit(2);
        }
        const char *msg = "hello";
        write(cli, msg, 5);
        sleep(3);
        close(cli);
        _exit(0);
    }

    /* parent: maxevents=4 deliberately exposes the duplicate-event bug.
     * Pre-fix this returns n=4 with out[0..4] all pointing at srv. */
    struct epoll_event out[4];
    memset(out, 0, sizeof(out));
    int n = epoll_wait(ep, out, 4, 3000);
    CHECK(n == 1 && (out[0].events & EPOLLIN), "listen_epoll_fires_once");

    int acc = accept(srv, NULL, NULL);
    CHECK(acc >= 0, "accept");

    ev.events = EPOLLIN;
    ev.data.fd = acc;
    CHECK(epoll_ctl(ep, EPOLL_CTL_ADD, acc, &ev) == 0, "epoll_add_client");

    /* same shape on the accepted fd */
    memset(out, 0, sizeof(out));
    n = epoll_wait(ep, out, 4, 3000);
    CHECK(n == 1 && out[0].data.fd == acc && (out[0].events & EPOLLIN),
          "client_data_epoll_fires_once");

    char buf[32] = {0};
    ssize_t r = read(acc, buf, sizeof(buf) - 1);
    CHECK(r == 5 && strcmp(buf, "hello") == 0, "read_client_msg");

    close(acc);
    close(srv);
    close(ep);
    waitpid(pid, NULL, 0);
    unlink(path);

    printf("\n=== Results: epoll_lt ===\n  PASS: %d  FAIL: %d\n", pass, fail);
    if (fail == 0) {
        printf("DONE: %d pass, 0 fail\n", pass);
    } else {
        printf("FAIL: %d test(s) failed\n", fail);
    }
    return fail;
}
