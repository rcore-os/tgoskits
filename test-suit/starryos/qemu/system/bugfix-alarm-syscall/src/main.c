#define _GNU_SOURCE

/*
 * Linux x86_64 exposes alarm(2) as a syscall sharing ITIMER_REAL state with
 * setitimer(2). A pending subsecond timer must return one rather than zero so
 * callers cannot mistake it for a disarmed timer.
 */

#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <unistd.h>

static int fail(const char *operation)
{
    fprintf(
        stderr,
        "FAIL: %s errno=%d (%s)\n",
        operation,
        errno,
        strerror(errno)
    );
    puts("STARRY_GROUPED_TEST_FAILED: bugfix-alarm-syscall");
    return EXIT_FAILURE;
}

static int timer_is_disarmed(void)
{
    struct itimerval current = {0};
    if (getitimer(ITIMER_REAL, &current) < 0) {
        return 0;
    }
    return current.it_interval.tv_sec == 0 &&
           current.it_interval.tv_usec == 0 &&
           current.it_value.tv_sec == 0 &&
           current.it_value.tv_usec == 0;
}

static int timer_is_single_shot_near(unsigned int seconds)
{
    struct itimerval current = {0};
    if (getitimer(ITIMER_REAL, &current) < 0) {
        return 0;
    }
    if (current.it_interval.tv_sec != 0 || current.it_interval.tv_usec != 0) {
        return 0;
    }

    int64_t remaining_us =
        (int64_t)current.it_value.tv_sec * 1000000 + current.it_value.tv_usec;
    int64_t requested_us = (int64_t)seconds * 1000000;
    return remaining_us > 0 && remaining_us <= requested_us;
}

int main(void)
{
#ifndef SYS_alarm
    puts("SKIP: SYS_alarm is unavailable on this architecture");
    puts("STARRY_GROUPED_TEST_PASSED: bugfix-alarm-syscall");
    return EXIT_SUCCESS;
#else
    struct itimerval timer = {0};
    if (setitimer(ITIMER_REAL, &timer, NULL) < 0) {
        return fail("reset ITIMER_REAL");
    }

    errno = 0;
    long previous = syscall(SYS_alarm, 5U);
    if (previous != 0) {
        errno = previous < 0 ? errno : EPROTO;
        return fail("arm alarm from disarmed state");
    }
    if (!timer_is_single_shot_near(5U)) {
        errno = EPROTO;
        return fail("inspect armed alarm");
    }

    previous = syscall(SYS_alarm, 0U);
    if (previous < 1 || previous > 5) {
        errno = EPROTO;
        return fail("cancel alarm and return remaining seconds");
    }
    if (!timer_is_disarmed()) {
        errno = EPROTO;
        return fail("verify alarm cancellation");
    }

    timer.it_value.tv_usec = 250000;
    if (setitimer(ITIMER_REAL, &timer, NULL) < 0) {
        return fail("arm fractional ITIMER_REAL");
    }

    previous = syscall(SYS_alarm, 7U);
    if (previous != 1) {
        errno = EPROTO;
        return fail("preserve a pending subsecond remainder");
    }
    if (!timer_is_single_shot_near(7U)) {
        errno = EPROTO;
        return fail("verify alarm replaces ITIMER_REAL");
    }

    previous = syscall(SYS_alarm, 0U);
    if (previous < 1 || previous > 7 || !timer_is_disarmed()) {
        errno = EPROTO;
        return fail("final alarm cleanup");
    }

    puts("STARRY_ALARM_SYSCALL_PASSED");
    puts("STARRY_GROUPED_TEST_PASSED: bugfix-alarm-syscall");
    return EXIT_SUCCESS;
#endif
}
