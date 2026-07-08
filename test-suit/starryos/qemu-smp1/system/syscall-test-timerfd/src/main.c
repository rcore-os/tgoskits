/* timerfd regression test.
 *
 * Exercises:
 *   - timerfd_create(CLOCK_MONOTONIC, 0)
 *   - timerfd_settime with a 100ms one-shot
 *   - read returns u64 = 1 (one expiration)
 *   - timerfd_settime with 50ms repeating interval
 *   - after ~200ms at least 3 expirations are delivered
 *   - poll() reports readable after the first tick
 *   - CLOCK_REALTIME + TFD_TIMER_ABSTIME is accepted and fires
 */
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <sys/timerfd.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

int main(void) {
    TEST_START("timerfd");

    int fd = timerfd_create(CLOCK_MONOTONIC, 0);
    CHECK(fd >= 0, "create CLOCK_MONOTONIC");

    /* one-shot, 100 ms */
    struct itimerspec spec = {
        .it_interval = {0, 0},
        .it_value    = {0, 100 * 1000 * 1000},
    };
    CHECK_RET(timerfd_settime(fd, 0, &spec, NULL), 0, "settime oneshot 100ms");

    uint64_t expirations = 0;
    ssize_t n = read(fd, &expirations, sizeof(expirations));
    CHECK(n == (ssize_t)sizeof(expirations), "read oneshot returned 8 bytes");
    CHECK(expirations == 1, "oneshot expiration count == 1");

    /* periodic, 50 ms */
    struct itimerspec periodic = {
        .it_interval = {0, 50 * 1000 * 1000},
        .it_value    = {0, 50 * 1000 * 1000},
    };
    CHECK_RET(timerfd_settime(fd, 0, &periodic, NULL), 0, "settime periodic 50ms");

    /* poll() should become readable */
    struct pollfd pf = {.fd = fd, .events = POLLIN};
    int p = poll(&pf, 1, 500);
    CHECK(p == 1 && (pf.revents & POLLIN), "poll() reports POLLIN");

    /* let it run a bit longer then read */
    struct timespec sleep_for = {0, 200 * 1000 * 1000};
    nanosleep(&sleep_for, NULL);

    expirations = 0;
    n = read(fd, &expirations, sizeof(expirations));
    CHECK(n == (ssize_t)sizeof(expirations), "read periodic returned 8 bytes");
    CHECK(expirations >= 3, "periodic expiration count >= 3");

    /* disarm */
    struct itimerspec disarm = {0};
    CHECK_RET(timerfd_settime(fd, 0, &disarm, NULL), 0, "settime disarm");

    close(fd);

    /* CLOCK_REALTIME + TFD_TIMER_ABSTIME: pass an absolute deadline 100 ms
     * past `now` (read via clock_gettime). On Linux this fires at that
     * wall-clock instant; this kernel maps wall_time onto the monotonic
     * timebase, so the deadline still falls ~100 ms in the future and the
     * timer fires once. The point of this check is the syscall is accepted
     * and produces an expiration — not the EINVAL the previous version
     * locked in. */
    int rfd = timerfd_create(CLOCK_REALTIME, 0);
    CHECK(rfd >= 0, "create CLOCK_REALTIME");

    struct timespec now_ts = {0};
    CHECK_RET(clock_gettime(CLOCK_REALTIME, &now_ts), 0, "clock_gettime REALTIME");

    long deadline_nsec = now_ts.tv_nsec + 100 * 1000 * 1000;
    struct itimerspec abs_spec = {
        .it_interval = {0, 0},
        .it_value    = {
            .tv_sec  = now_ts.tv_sec + deadline_nsec / 1000000000,
            .tv_nsec = deadline_nsec % 1000000000,
        },
    };
    CHECK_RET(timerfd_settime(rfd, TFD_TIMER_ABSTIME, &abs_spec, NULL), 0,
              "REALTIME + ABSTIME settime succeeds");

    uint64_t abs_expirations = 0;
    ssize_t abs_n = read(rfd, &abs_expirations, sizeof(abs_expirations));
    CHECK(abs_n == (ssize_t)sizeof(abs_expirations),
          "REALTIME + ABSTIME read returned 8 bytes");
    CHECK(abs_expirations >= 1, "REALTIME + ABSTIME expiration count >= 1");
    close(rfd);

    /* Regression: timerfd_settime() must clear accumulated expirations.
     * Linux's man timerfd_read(2): "the number of expirations that have
     * occurred ... since the last successful read or since the last
     * timerfd_settime() that reset the timer." Without the reset, an
     * unread rearm would let the next read return ticks from the old
     * setting. */
    int sfd = timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC);
    CHECK(sfd >= 0, "create MONOTONIC for settime-reset test");

    struct itimerspec quick = {
        .it_interval = {0, 0},
        .it_value    = {0, 50 * 1000 * 1000},   /* 50 ms */
    };
    CHECK_RET(timerfd_settime(sfd, 0, &quick, NULL), 0, "settime quick");

    /* Wait past the deadline so an expiration is queued, but do NOT read. */
    usleep(150 * 1000);

    /* Re-arm with a long (1 second) one-shot. After this settime the
     * stale "1 expiration" from the quick timer must be discarded. */
    struct itimerspec slow = {
        .it_interval = {0, 0},
        .it_value    = {1, 0},
    };
    CHECK_RET(timerfd_settime(sfd, 0, &slow, NULL), 0, "settime slow rearm");

    /* Non-blocking read: the new timer has not fired yet, so the read
     * must report EAGAIN. If the kernel kept the stale count, this
     * read would succeed and return 1. */
    int fl = fcntl(sfd, F_GETFL, 0);
    CHECK(fl != -1, "F_GETFL on timerfd");
    CHECK_RET(fcntl(sfd, F_SETFL, fl | O_NONBLOCK), 0, "F_SETFL O_NONBLOCK");

    uint64_t after_rearm = 0;
    errno = 0;
    ssize_t rn = read(sfd, &after_rearm, sizeof(after_rearm));
    CHECK(rn == -1 && (errno == EAGAIN || errno == EWOULDBLOCK),
          "read after settime-rearm returns EAGAIN (stale ticks discarded)");

    /* Disarm path: same contract. */
    CHECK_RET(timerfd_settime(sfd, 0, &quick, NULL), 0, "settime quick (2)");
    usleep(150 * 1000);
    struct itimerspec disarm_spec = {{0, 0}, {0, 0}};
    CHECK_RET(timerfd_settime(sfd, 0, &disarm_spec, NULL), 0, "settime disarm");
    errno = 0;
    rn = read(sfd, &after_rearm, sizeof(after_rearm));
    CHECK(rn == -1 && (errno == EAGAIN || errno == EWOULDBLOCK),
          "read after disarm returns EAGAIN (stale ticks discarded)");
    close(sfd);

    /* Regression: CLOCK_MONOTONIC + TFD_TIMER_ABSTIME must interpret
     * the absolute deadline in the monotonic domain. Earlier kernels
     * pretended the user passed a wall-clock timestamp and almost
     * always fired immediately. */
    int mfd = timerfd_create(CLOCK_MONOTONIC, 0);
    CHECK(mfd >= 0, "create CLOCK_MONOTONIC for ABSTIME test");

    struct timespec mono_now = {0};
    CHECK_RET(clock_gettime(CLOCK_MONOTONIC, &mono_now), 0, "clock_gettime MONOTONIC");

    long mono_deadline_nsec = mono_now.tv_nsec + 150 * 1000 * 1000;
    struct itimerspec mono_abs = {
        .it_interval = {0, 0},
        .it_value    = {
            .tv_sec  = mono_now.tv_sec + mono_deadline_nsec / 1000000000,
            .tv_nsec = mono_deadline_nsec % 1000000000,
        },
    };
    CHECK_RET(timerfd_settime(mfd, TFD_TIMER_ABSTIME, &mono_abs, NULL), 0,
              "MONOTONIC + ABSTIME settime succeeds");

    /* The timer is 150ms in the future. A non-blocking read right
     * after settime must return EAGAIN — if the kernel interpreted
     * the deadline as a wall-clock time, the deadline would be far
     * in the past and the read would succeed immediately. */
    int mfl = fcntl(mfd, F_GETFL, 0);
    CHECK(mfl != -1, "F_GETFL mfd");
    CHECK_RET(fcntl(mfd, F_SETFL, mfl | O_NONBLOCK), 0, "F_SETFL O_NONBLOCK mfd");
    uint64_t mexp = 0;
    errno = 0;
    ssize_t mrn = read(mfd, &mexp, sizeof(mexp));
    CHECK(mrn == -1 && (errno == EAGAIN || errno == EWOULDBLOCK),
          "MONOTONIC + ABSTIME read returns EAGAIN before deadline");

    /* Restore blocking and wait it out. */
    CHECK_RET(fcntl(mfd, F_SETFL, mfl), 0, "F_SETFL restore");
    mrn = read(mfd, &mexp, sizeof(mexp));
    CHECK(mrn == (ssize_t)sizeof(mexp), "MONOTONIC + ABSTIME read returns 8 bytes");
    CHECK(mexp >= 1, "MONOTONIC + ABSTIME expiration count >= 1");
    close(mfd);

    /* Regression: read() with a bad buffer must not consume the
     * expiration count. Linux preserves the count when the copyout
     * fails (EFAULT). The previous implementation did
     * `swap(0)` before the copy and lost the tick. */
    int bfd = timerfd_create(CLOCK_MONOTONIC, 0);
    CHECK(bfd >= 0, "create MONOTONIC for bad-buffer test");
    struct itimerspec quick2 = {
        .it_interval = {0, 0},
        .it_value    = {0, 50 * 1000 * 1000},
    };
    CHECK_RET(timerfd_settime(bfd, 0, &quick2, NULL), 0, "settime quick (bad-buffer)");

    /* Wait until at least one tick is queued. */
    {
        struct pollfd bpf = {.fd = bfd, .events = POLLIN};
        int pr = poll(&bpf, 1, 500);
        CHECK(pr == 1 && (bpf.revents & POLLIN),
              "bfd is readable before bad-buffer read");
    }

    /* Reading into a bad pointer must EFAULT and leave the tick. */
    errno = 0;
    ssize_t bad = read(bfd, (void *)1, sizeof(uint64_t));
    CHECK(bad == -1 && errno == EFAULT,
          "read into bad buffer returns EFAULT");

    /* Now a valid read must still see the preserved expiration. */
    uint64_t bexp = 0;
    ssize_t good = read(bfd, &bexp, sizeof(bexp));
    CHECK(good == (ssize_t)sizeof(bexp),
          "valid read after EFAULT recovers 8 bytes");
    CHECK(bexp >= 1, "preserved expiration count >= 1 after failed read");
    close(bfd);

    /* Concurrent-reader single-consumer semantics: a pending expiration must
     * be claimed in full by exactly one read(). Use a one-shot timer so no
     * new tick can appear between the parent and child reads and make the
     * test result depend only on timerfd count consumption. */
    int cfd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    CHECK(cfd >= 0, "create CLOCK_MONOTONIC TFD_NONBLOCK for concurrency");
    struct itimerspec cspec = {
        .it_interval = {0, 0},
        .it_value    = {0, 10 * 1000 * 1000},
    };
    CHECK_RET(timerfd_settime(cfd, 0, &cspec, NULL), 0,
              "settime oneshot 10ms for concurrency");

    struct pollfd cpf = {.fd = cfd, .events = POLLIN};
    int cpr = poll(&cpf, 1, 500);
    CHECK(cpr == 1 && (cpf.revents & POLLIN),
          "one-shot concurrency timer became readable");

    int pipefd[2];
    CHECK_RET(pipe(pipefd), 0, "pipe for concurrent-reader coordination");

    pid_t pid = fork();
    CHECK(pid >= 0, "fork for concurrent reader");
    if (pid == 0) {
        close(pipefd[0]);
        uint64_t got = 0;
        ssize_t cn = read(cfd, &got, sizeof(got));
        if (cn != (ssize_t)sizeof(got)) {
            got = 0;
        }
        write(pipefd[1], &got, sizeof(got));
        close(pipefd[1]);
        _exit(0);
    }
    close(pipefd[1]);
    uint64_t parent_got = 0;
    ssize_t pn = read(cfd, &parent_got, sizeof(parent_got));
    if (pn != (ssize_t)sizeof(parent_got)) {
        parent_got = 0;
    }
    uint64_t child_got = 0;
    read(pipefd[0], &child_got, sizeof(child_got));
    close(pipefd[0]);
    waitpid(pid, NULL, 0);
    CHECK(parent_got + child_got == 1,
          "exactly one expiration consumed by concurrent readers");
    CHECK(parent_got == 0 || child_got == 0,
          "single-consumer: only one reader observed the expiration");
    /* Disarm before close so any deferred timer task stops touching cfd. */
    struct itimerspec cdisarm = {.it_interval = {0, 0}, .it_value = {0, 0}};
    timerfd_settime(cfd, 0, &cdisarm, NULL);
    close(cfd);

    TEST_DONE();
}
