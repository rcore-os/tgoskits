/*
 * bug-select-hup-writable — select(2) must not report a hung-up fd as writable.
 *
 * ground truth: Linux v7.2 fs/select.c POLLIN_SET/POLLOUT_SET. A hangup
 * (EPOLLHUP) is in POLLIN_SET (a read returns EOF) but NOT in POLLOUT_SET, so
 * after the write end of a pipe is closed:
 *   - select() for writability on the read end must time out (not writable);
 *   - select() for readability on the read end must be ready (EOF is readable).
 * StarryOS folded the ERR|HUP "always report" mask into both directions, so it
 * spuriously reported the read end as writable.
 */
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <sys/select.h>

static int failed;

static void check(int cond, const char *msg)
{
    if (cond) {
        printf("  PASS | %s\n", msg);
    } else {
        printf("  FAIL | %s | errno=%d (%s)\n", msg, errno, strerror(errno));
        failed = 1;
    }
}

int main(void)
{
    /* case 1: write end closed -> read end is NOT writable, select() times out. */
    int pfd[2];
    if (pipe(pfd) < 0) { perror("pipe"); return 2; }
    close(pfd[1]);

    fd_set wfds;
    FD_ZERO(&wfds);
    FD_SET(pfd[0], &wfds);
    struct timeval tv = { .tv_sec = 0, .tv_usec = 200000 };
    int r = select(pfd[0] + 1, NULL, &wfds, NULL, &tv);
    check(r == 0 && !FD_ISSET(pfd[0], &wfds),
          "写端关闭后 select(write) 读端超时且不就绪(HUP 不算可写)");

    /* case 2: the same hangup must still make the read end readable (EOF). */
    fd_set rfds;
    FD_ZERO(&rfds);
    FD_SET(pfd[0], &rfds);
    tv.tv_sec = 0;
    tv.tv_usec = 200000;
    r = select(pfd[0] + 1, &rfds, NULL, NULL, &tv);
    check(r == 1 && FD_ISSET(pfd[0], &rfds),
          "写端关闭后 select(read) 读端就绪(HUP 可读, read 返回 EOF)");

    close(pfd[0]);
    printf("=== bug-select-hup-writable: %s ===\n", failed ? "FAIL" : "PASS");
    return failed;
}
