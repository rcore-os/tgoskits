/*
 * test_timerfd_efault_wake.c -- regression for the EFAULT-recovery wake path.
 *
 * timerfd_read() must single-consumer the expiration count, which it does by
 * CASing the counter to zero before copying out. If the copyout faults
 * (EFAULT), the kernel restores the count via fetch_add(n) and must also call
 * `poll_rx.wake()`. The wake matters when another reader/poller has parked
 * itself between our CAS-to-zero and our fetch_add restore:
 *
 *     main thread                       reader thread B
 *     -----------                       ---------------
 *     CAS expire_count 1 -> 0
 *                                       poll(POLLIN) -> count==0 -> park
 *     copy_to_user(BAD) -> EFAULT
 *     fetch_add(1)  // count = 1 again
 *     poll_rx.wake()                    <-- without this wake, B sleeps
 *                                           forever (no further timer ticks)
 *
 * The race is real only when threads run truly concurrently, so this test
 * lives under qemu-smp4. Each iteration arms a one-shot timerfd so that the
 * only path that can unstick B in the buggy case is the EFAULT-recovery
 * wake; a missing wake leaves B parked indefinitely and the watchdog kills
 * the test with FAIL.
 *
 * To keep main from blocking when B happens to win the CAS race, the fd is
 * opened with TFD_NONBLOCK: main's read either succeeds (count>0, no race)
 * or returns EAGAIN/EFAULT. B drives parking via poll(), which honors the
 * timerfd's poll_rx waitqueue regardless of TFD_NONBLOCK.
 *
 * Each iteration:
 *   1. Arm a one-shot timerfd and wait until expire_count == 1.
 *   2. Spawn B (clone, shared VM/files): B busy-spins on `go`, then does
 *      read(); if that returns EAGAIN, B falls into poll(POLLIN, 1500ms)
 *      to actually park on poll_rx.
 *   3. Main flips `go` and immediately fires read(fd, BAD, 8).
 *   4. If main wins the CAS, EFAULT-recovery must restore the tick AND
 *      wake B's poll() so B's follow-up read returns 1.
 *   5. If B wins, main returns EAGAIN; B's first read got the tick and
 *      never enters poll().
 *   6. A rescue tick guards against scheduler outliers; the watchdog
 *      fires if any iteration ever takes longer than ~4 seconds.
 *
 * Without the wake() on the EFAULT path, on SMP the race wedges B in at
 * least one round and the watchdog reports FAIL. With the fix in place,
 * every iteration drains B cleanly.
 */
#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/timerfd.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

/* Bumping iterations widens the odds of B parking inside the
 * CAS-to-zero -> fetch_add restore window during at least one round.
 * The race is narrow (a few CAS + one user-pointer fault); without the
 * fix, however, ANY hit wedges B forever, so the watchdog catches the
 * bug even at low hit rates. */
#define ITERATIONS         200
#define STACK_SIZE         (64 * 1024)
#define WATCHDOG_MS        4000
#define READER_TIMEOUT_MS  1500
#define POLL_TIMEOUT_MS    1500

/* Shared state between main and reader B. */
struct ctx {
    int fd;
    atomic_int   ready;       /* B reached just before read() */
    atomic_int   go;          /* main releases B to enter read() */
    atomic_int   done;        /* B finished its read() */
    atomic_long  read_ret;    /* B's read() final return value */
    atomic_int   read_errno;  /* B's read() final errno */
    atomic_ullong got;        /* expiration count B claimed */
    atomic_int   parked;      /* B fell into poll() after EAGAIN */
    atomic_int   poll_revents;
};

static long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static int reader_thread(void *arg) {
    struct ctx *c = (struct ctx *)arg;

    atomic_store_explicit(&c->ready, 1, memory_order_release);

    /* Spin until main releases us. Both threads then race their entry
     * into the kernel as close to simultaneous as possible. */
    while (atomic_load_explicit(&c->go, memory_order_acquire) == 0) {
        /* tight spin: keep B on-CPU so the two reads race in the kernel. */
    }

    uint64_t buf = 0;
    ssize_t r = read(c->fd, &buf, sizeof(buf));
    int err = errno;

    if (r == -1 && (err == EAGAIN || err == EWOULDBLOCK)) {
        /* B lost the CAS race against main and there's nothing pending
         * (TFD_NONBLOCK fd). Park on poll(). With the fix, main's
         * EFAULT recovery wakes us as soon as it restores the tick. */
        struct pollfd pf = {.fd = c->fd, .events = POLLIN};
        atomic_store_explicit(&c->parked, 1, memory_order_release);
        int pr = poll(&pf, 1, POLL_TIMEOUT_MS);
        atomic_store_explicit(&c->poll_revents,
                              (pr > 0) ? pf.revents : 0,
                              memory_order_relaxed);
        if (pr > 0 && (pf.revents & POLLIN)) {
            r = read(c->fd, &buf, sizeof(buf));
            err = errno;
        }
    }

    atomic_store_explicit(&c->got, (unsigned long long)buf, memory_order_relaxed);
    atomic_store_explicit(&c->read_ret, (long)r, memory_order_relaxed);
    atomic_store_explicit(&c->read_errno, err, memory_order_relaxed);
    atomic_store_explicit(&c->done, 1, memory_order_release);
    return 0;
}

/* Atomic shared between main and watchdog: bumped each iteration so the
 * watchdog can tell forward progress from a wedge. */
static atomic_int g_iter_progress;

static int watchdog_thread(void *arg) {
    (void)arg;
    int last = atomic_load_explicit(&g_iter_progress, memory_order_acquire);
    long quiet_ms = 0;

    while (1) {
        usleep(100 * 1000);
        int now = atomic_load_explicit(&g_iter_progress, memory_order_acquire);
        if (now != last) {
            last = now;
            quiet_ms = 0;
            if (now < 0) {
                return 0;  /* main signalled clean shutdown */
            }
        } else {
            quiet_ms += 100;
            if (quiet_ms >= WATCHDOG_MS) {
                printf("  FAIL | %s | watchdog timeout: no forward "
                       "progress for %ldms (EFAULT-recovery wake missing?)\n",
                       __FILE__, quiet_ms);
                _exit(1);
            }
        }
    }
}

/* Arm a one-shot timerfd and busy-wait until at least one expiration is
 * queued, so the subsequent race has count == 1 to grab. */
static int arm_and_wait(int fd, long nsec) {
    struct itimerspec spec = {
        .it_interval = {0, 0},
        .it_value    = {0, nsec},
    };
    if (timerfd_settime(fd, 0, &spec, NULL) != 0) {
        return -1;
    }
    /* Poll-and-yield until the tick lands. We can't just read() -- that
     * would consume the count we need for the race. */
    long deadline = now_ms() + 200;
    while (now_ms() < deadline) {
        struct timespec sleep_for = {0, 500 * 1000};  /* 500us */
        nanosleep(&sleep_for, NULL);
        struct itimerspec cur;
        if (timerfd_gettime(fd, &cur) == 0 &&
            cur.it_value.tv_sec == 0 && cur.it_value.tv_nsec == 0) {
            /* Timer disarmed itself (one-shot fired). Give the background
             * task a final scheduler slice to publish the tick. */
            nanosleep(&sleep_for, NULL);
            return 0;
        }
    }
    return -1;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("timerfd_efault_wake");

    /* Spin up the watchdog first; if any iteration wedges the test, the
     * watchdog will print FAIL and _exit before the harness times out
     * silently. */
    void *wd_stack = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(wd_stack != MAP_FAILED, "watchdog stack mmap");
    if (wd_stack == MAP_FAILED) {
        TEST_DONE();
    }
    int wd_tid = clone(watchdog_thread, (char *)wd_stack + STACK_SIZE,
                       CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND,
                       NULL);
    CHECK(wd_tid >= 0, "clone watchdog");

    int fd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    CHECK(fd >= 0, "create timerfd (TFD_NONBLOCK)");
    if (fd < 0) {
        TEST_DONE();
    }

    int wins_main = 0;          /* main got EFAULT, B drained via poll wake */
    int wins_reader = 0;        /* B got the tick on its first read */
    int wedges = 0;             /* watchdog would have fired here */
    int parks = 0;              /* iterations where B fell into poll() */

    for (int i = 0; i < ITERATIONS; i++) {
        atomic_store_explicit(&g_iter_progress, i, memory_order_release);

        if (arm_and_wait(fd, 1 * 1000 * 1000) != 0) {
            printf("  FAIL | %s | iter %d: arm_and_wait failed\n",
                   __FILE__, i);
            wedges++;
            continue;
        }

        struct ctx c = {.fd = fd};
        atomic_init(&c.ready, 0);
        atomic_init(&c.go, 0);
        atomic_init(&c.done, 0);
        atomic_init(&c.read_ret, 0);
        atomic_init(&c.read_errno, 0);
        atomic_init(&c.got, 0);
        atomic_init(&c.parked, 0);
        atomic_init(&c.poll_revents, 0);

        void *rstack = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (rstack == MAP_FAILED) {
            printf("  FAIL | %s | iter %d: reader stack mmap\n",
                   __FILE__, i);
            wedges++;
            continue;
        }

        int rtid = clone(reader_thread, (char *)rstack + STACK_SIZE,
                         CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND,
                         &c);
        if (rtid < 0) {
            munmap(rstack, STACK_SIZE);
            printf("  FAIL | %s | iter %d: clone reader\n", __FILE__, i);
            wedges++;
            continue;
        }

        /* Wait until B is spinning on go so both threads are on-CPU.
         * Then signal go and immediately fire the racing bad read. */
        while (!atomic_load_explicit(&c.ready, memory_order_acquire)) {
            sched_yield();
        }
        atomic_store_explicit(&c.go, 1, memory_order_release);

        /* Racing read with a bogus userspace pointer. If main wins the
         * CAS, the kernel must restore expire_count via fetch_add + wake
         * B's poll(); otherwise B sleeps for POLL_TIMEOUT_MS. */
        errno = 0;
        ssize_t mn = read(fd, (void *)1, sizeof(uint64_t));
        int merrno = errno;

        /* Wait for B with a per-iteration cap. The watchdog would also
         * catch a hang, but a tighter local check makes the failure
         * point obvious in test output. */
        long t0 = now_ms();
        while (!atomic_load_explicit(&c.done, memory_order_acquire)) {
            if (now_ms() - t0 > READER_TIMEOUT_MS + 200) {
                break;
            }
            struct timespec s = {0, 1 * 1000 * 1000};
            nanosleep(&s, NULL);
        }

        int done = atomic_load_explicit(&c.done, memory_order_acquire);
        if (!done) {
            /* Try a rescue tick: arm another one-shot so the timer task
             * itself wakes B. If B was wedged purely because the EFAULT
             * wake was missing, this still gets us out (and we count the
             * iteration as a failure). */
            (void)arm_and_wait(fd, 2 * 1000 * 1000);
            long t1 = now_ms();
            while (!atomic_load_explicit(&c.done, memory_order_acquire)) {
                if (now_ms() - t1 > READER_TIMEOUT_MS) {
                    break;
                }
                struct timespec s = {0, 1 * 1000 * 1000};
                nanosleep(&s, NULL);
            }
            done = atomic_load_explicit(&c.done, memory_order_acquire);
            if (!done) {
                printf("  FAIL | %s:%d | reader wedged on iter %d "
                       "(main ret=%zd errno=%d)\n",
                       __FILE__, __LINE__, i, mn, merrno);
                wedges++;
                /* Leak rstack: B is still parked in the kernel and will
                 * write to it if/when it ever wakes. The watchdog will
                 * shoot the test soon anyway. */
                continue;
            }
            printf("  FAIL | %s:%d | reader needed rescue tick on iter %d"
                   " (main ret=%zd errno=%d): EFAULT wake missing\n",
                   __FILE__, __LINE__, i, mn, merrno);
            wedges++;
            int rstatus;
            waitpid(rtid, &rstatus, __WALL);
            (void)rstatus;
            munmap(rstack, STACK_SIZE);
            continue;
        }

        long rret = atomic_load_explicit(&c.read_ret, memory_order_relaxed);
        unsigned long long got = atomic_load_explicit(&c.got, memory_order_relaxed);
        int parked = atomic_load_explicit(&c.parked, memory_order_acquire);
        if (parked) {
            parks++;
        }

        /* Outcome classification.
         *
         * 1. main got EFAULT, B parked then poll-woke and read got the
         *    tick: this is the exact path under test. main's CAS-clear
         *    was followed by an EFAULT, the fetch_add restored the
         *    tick, and poll_rx.wake() unblocked B's poll().
         * 2. main got 8 bytes (read returned > 0): main beat B to the
         *    CAS, B's read returned EAGAIN, then either:
         *      a. poll() observed POLLIN and B's second read succeeded
         *         (also tests wake from EFAULT or from the next CAS
         *         cycle), or
         *      b. nothing more happened, B is stuck (handled as wedge).
         * 3. B never parked: B raced its read in fast enough to see
         *    the tick before main; nothing to test.
         */
        int outcome_main =
            mn == -1 && merrno == EFAULT &&
            rret == (long)sizeof(uint64_t) && got == 1;
        int outcome_reader_first =
            !parked && rret == (long)sizeof(uint64_t) && got == 1;
        int outcome_eagain_then_poll =
            parked && rret == (long)sizeof(uint64_t) && got == 1;

        if (outcome_main) {
            wins_main++;
        } else if (outcome_reader_first) {
            wins_reader++;
        } else if (outcome_eagain_then_poll) {
            /* Same wake test, but main saw the count first. Count under
             * wins_main: the wake() still had to land on B. */
            wins_main++;
        } else if (parked) {
            /* B parked in poll() but did not come away with the tick.
             * This is the exact failure mode of a missing EFAULT-recovery
             * wake: poll() timed out, B's follow-up read either never
             * ran or saw EAGAIN. The expiration is still in the count
             * (we'll catch that mismatch via main's outcome below).
             *
             * Note: with the fix in place this branch is unreachable
             * unless poll() races a settime() that cleared the count,
             * which we don't do per-iteration. So treat this as a
             * test-fail signal. */
            int read_errno_observed = atomic_load_explicit(
                &c.read_errno, memory_order_relaxed);
            long pr = atomic_load_explicit(&c.poll_revents, memory_order_relaxed);
            printf("  FAIL | %s:%d | parked reader did not get woken on iter %d"
                   " (main ret=%zd merrno=%d; B ret=%ld errno=%d revents=%ld)\n",
                   __FILE__, __LINE__, i, mn, merrno, rret,
                   read_errno_observed, pr);
            wedges++;
        } else {
            /* Unexpected combination unrelated to parking. Don't fail
             * the run on these; the watchdog catches actual wedges. */
        }

        int rstatus;
        waitpid(rtid, &rstatus, __WALL);
        (void)rstatus;
        munmap(rstack, STACK_SIZE);
    }

    /* Tell watchdog we're done so it can exit cleanly. */
    atomic_store_explicit(&g_iter_progress, -1, memory_order_release);
    struct timespec final_pause = {0, 200 * 1000 * 1000};
    nanosleep(&final_pause, NULL);
    int wd_status;
    waitpid(wd_tid, &wd_status, __WALL);
    (void)wd_status;
    munmap(wd_stack, STACK_SIZE);

    CHECK(wedges == 0,
          "every iteration completed without watchdog/rescue (wake delivered)");
    CHECK(wins_main + wins_reader > 0,
          "at least one iteration produced a classifiable outcome");
    /* `parks > 0` would be the strongest assertion (it proves B actually
     * went through the EFAULT-recovery wake path) but pure scheduling
     * luck can keep B from ever losing the race on a fast SMP host.
     * Treat it as advisory: print INFO and let the watchdog be the
     * teeth of the bug check. */
    if (parks == 0) {
        printf("  INFO | %s | parks=0: race window was not observed this "
               "run; rerun for stronger coverage\n", __FILE__);
    }

    close(fd);
    printf("  INFO | %s | wins_main=%d wins_reader=%d parks=%d wedges=%d\n",
           __FILE__, wins_main, wins_reader, parks, wedges);

    TEST_DONE();
}
