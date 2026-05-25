/*
 * bug-open-eintr-not-implemented: blocking open(FIFO) interrupted by signal
 * (no SA_RESTART) should fail with -1 EINTR. Starry doesn't deliver EINTR.
 *
 * man 2 open §"EINTR":
 *   "EINTR — While blocked waiting to complete an open of a slow device
 *    (e.g., a FIFO; see fifo(7)), the call was interrupted by a signal
 *    handler; see signal(7)."
 *
 * Linux behavior (host/WSL2 verified):
 *   FIFO O_RDONLY no writer + alarm(1) + SIGALRM(no SA_RESTART) → -1 EINTR
 * StarryOS bug: signal doesn't interrupt the blocking open, or open returns
 *   fd>=0 anyway.
 *
 * Note: requires starry signal subsystem to deliver signals to processes
 *   blocked in syscall path AND handle EINTR return. May relate to FIFO
 *   reader_count tracking (FIFO subsystem is incomplete).
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static volatile sig_atomic_t alrm_fired = 0;
static void alrm_handler(int sig) { (void)sig; alrm_fired = 1; }

int main(void)
{
    const char *fifo = "/tmp/bug_eintr_fifo";
    unlink(fifo);
    if (mkfifo(fifo, 0644) != 0) { perror("mkfifo"); return 1; }

    struct sigaction sa = {0};
    sa.sa_handler = alrm_handler;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;  /* no SA_RESTART */
    sigaction(SIGALRM, &sa, NULL);

    alarm(2);  /* 2 sec to allow blocking open to be interrupted */
    errno = 0;
    int fd = open(fifo, O_RDONLY);  /* blocks waiting for writer */
    int err = errno;
    alarm(0);

    int ok = (fd == -1 && err == EINTR);
    if (ok) {
        printf("PASS: open(FIFO RDONLY, no writer) interrupted by SIGALRM -> -1 EINTR\n");
    } else {
        printf("FAIL: expected -1 EINTR, got fd=%d errno=%d (%s) alrm_fired=%d\n",
               fd, err, strerror(err), alrm_fired);
    }
    if (fd >= 0) close(fd);
    unlink(fifo);
    return ok ? 0 : 1;
}
