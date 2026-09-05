#define _GNU_SOURCE

#include <errno.h>
#include <poll.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/timerfd.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define LINUX_TIME_UPTIME_SEC_MAX (30LL * 365 * 24 * 60 * 60)
#define LINUX_TIME_SETTOD_SEC_MAX                                           \
    (INT64_MAX / 1000000000LL - LINUX_TIME_UPTIME_SEC_MAX)
#define CLOCK_STEP_SEC 120LL

static const char *timestamp_path = "/tmp/starry-clock-settime-timestamp";

struct timerfd_probes {
    int relative_fd;
    int cancel_fd;
    int cancel_periodic_fd;
};

struct posix_timer_probe {
    timer_t timer_id;
    sigset_t wait_set;
    sigset_t old_mask;
    int created;
    int mask_saved;
};

static int fail(const char *operation)
{
    fprintf(stderr, "FAIL: %s errno=%d (%s)\n", operation, errno,
            strerror(errno));
    puts("STARRY_GROUPED_TEST_FAILED: syscall-test-clock-settime");
    return EXIT_FAILURE;
}

static int64_t timespec_to_nanos(const struct timespec *time)
{
    return (int64_t)time->tv_sec * 1000000000LL + time->tv_nsec;
}

static struct timespec nanos_to_timespec(int64_t nanos)
{
    return (struct timespec){
        .tv_sec = nanos / 1000000000LL,
        .tv_nsec = nanos % 1000000000LL,
    };
}

static int expect_errno(long result, int expected, const char *operation)
{
    if (result == -1 && errno == expected) {
        return 0;
    }
    errno = EPROTO;
    return fail(operation);
}

static int check_unprivileged_set_is_rejected(const struct timespec *requested)
{
    pid_t child = fork();
    if (child < 0) {
        return fail("fork unprivileged clock setter");
    }
    if (child == 0) {
        if (setuid(65534) < 0) {
            _exit(2);
        }
        errno = 0;
        long result = syscall(SYS_clock_settime, CLOCK_REALTIME, requested);
        _exit(result == -1 && errno == EPERM ? 0 : 3);
    }

    int status = 0;
    if (waitpid(child, &status, 0) != child) {
        return fail("wait for unprivileged clock setter");
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        errno = EPROTO;
        return fail("reject unprivileged clock setter with EPERM");
    }
    return 0;
}

static int check_realtime_observers(int64_t requested_nanos,
                                    int64_t monotonic_before_nanos)
{
    struct timespec realtime = {0};
    struct timespec monotonic = {0};
    struct timeval wall = {0};

    if (clock_gettime(CLOCK_REALTIME, &realtime) < 0 ||
        clock_gettime(CLOCK_MONOTONIC, &monotonic) < 0 ||
        gettimeofday(&wall, NULL) < 0) {
        return fail("read clocks after clock_settime");
    }

    int64_t realtime_delta = timespec_to_nanos(&realtime) - requested_nanos;
    int64_t monotonic_delta =
        timespec_to_nanos(&monotonic) - monotonic_before_nanos;
    int64_t timeval_nanos =
        (int64_t)wall.tv_sec * 1000000000LL + wall.tv_usec * 1000LL;
    int64_t timeval_delta = timeval_nanos - requested_nanos;
    if (realtime_delta < 0 || realtime_delta > 5000000000LL ||
        monotonic_delta < 0 || monotonic_delta > 5000000000LL ||
        timeval_delta < 0 || timeval_delta > 5000000000LL) {
        errno = EPROTO;
        return fail("observe stepped realtime without changing monotonic time");
    }
    return 0;
}

static int check_new_file_timestamp(int64_t requested_nanos)
{
    unlink(timestamp_path);
    int fd = open(timestamp_path, O_CREAT | O_EXCL | O_WRONLY, 0600);
    if (fd < 0) {
        return fail("create timestamp probe");
    }
    if (write(fd, "x", 1) != 1) {
        close(fd);
        unlink(timestamp_path);
        return fail("write timestamp probe");
    }
    if (close(fd) < 0) {
        unlink(timestamp_path);
        return fail("close timestamp probe");
    }

    struct stat metadata = {0};
    int stat_result = stat(timestamp_path, &metadata);
    int saved_errno = errno;
    unlink(timestamp_path);
    errno = saved_errno;
    if (stat_result < 0) {
        return fail("stat timestamp probe");
    }

    int64_t timestamp_delta =
        timespec_to_nanos(&metadata.st_mtim) - requested_nanos;
    if (timestamp_delta < -1000000000LL ||
        timestamp_delta > 5000000000LL) {
        fprintf(stderr,
                "timestamp probe: requested=%lld mtime=%lld delta=%lld\n",
                (long long)requested_nanos,
                (long long)timespec_to_nanos(&metadata.st_mtim),
                (long long)timestamp_delta);
        errno = EPROTO;
        return fail("stamp new files from the adjusted realtime clock");
    }
    return 0;
}

static int prepare_timerfd_probes(const struct timespec *original_realtime,
                                  struct timerfd_probes *probes)
{
    probes->relative_fd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    probes->cancel_fd = timerfd_create(CLOCK_REALTIME, TFD_NONBLOCK);
    probes->cancel_periodic_fd = timerfd_create(CLOCK_REALTIME, TFD_NONBLOCK);
    if (probes->relative_fd < 0 || probes->cancel_fd < 0 ||
        probes->cancel_periodic_fd < 0) {
        return fail("create timerfd clock-step probes");
    }

    struct itimerspec relative = {
        .it_value = {.tv_sec = 30, .tv_nsec = 0},
    };
    struct itimerspec cancel = {
        .it_value = {
            .tv_sec = original_realtime->tv_sec + 600,
            .tv_nsec = original_realtime->tv_nsec,
        },
    };
    struct itimerspec cancel_periodic = {
        .it_interval = {.tv_sec = 0, .tv_nsec = 10000000},
        .it_value = {
            .tv_sec = original_realtime->tv_sec + 1,
            .tv_nsec = original_realtime->tv_nsec,
        },
    };
    if (timerfd_settime(probes->cancel_periodic_fd,
                        TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET,
                        &cancel_periodic, NULL) < 0 ||
        timerfd_settime(probes->relative_fd, 0, &relative, NULL) < 0 ||
        timerfd_settime(probes->cancel_fd,
                        TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET, &cancel,
                        NULL) < 0) {
        return fail("arm timerfd clock-step probes");
    }
    return 0;
}

static int check_timerfd_clock_step(const struct timerfd_probes *probes)
{
    uint64_t expirations = 0;
    usleep(100000);

    errno = 0;
    if (read(probes->relative_fd, &expirations, sizeof(expirations)) != -1 ||
        errno != EAGAIN) {
        errno = EPROTO;
        return fail("keep a relative timerfd on the monotonic clock");
    }

    errno = 0;
    if (read(probes->cancel_fd, &expirations, sizeof(expirations)) != -1 ||
        errno != ECANCELED) {
        errno = EPROTO;
        return fail("cancel an absolute realtime timerfd after clock step");
    }

    errno = 0;
    if (read(probes->cancel_periodic_fd, &expirations, sizeof(expirations)) != -1 ||
        errno != ECANCELED) {
        errno = EPROTO;
        return fail("cancel an expired periodic realtime timerfd");
    }
    usleep(100000);
    errno = 0;
    if (read(probes->cancel_periodic_fd, &expirations, sizeof(expirations)) != -1 ||
        errno != EAGAIN) {
        errno = EPROTO;
        return fail("keep canceled periodic timerfd disarmed until explicit rearm");
    }
    struct itimerspec current = {0};
    if (timerfd_gettime(probes->cancel_periodic_fd, &current) < 0 ||
        timespec_to_nanos(&current.it_value) != 0) {
        errno = EPROTO;
        return fail("report no pending deadline after canceled expiration");
    }
    if (timerfd_gettime(probes->cancel_fd, &current) < 0 ||
        current.it_value.tv_sec < 400) {
        errno = EPROTO;
        return fail("preserve the original unexpired timer after cancellation read");
    }
    struct itimerspec rearm = {.it_value = {.tv_nsec = 20000000}};
    if (timerfd_settime(probes->cancel_periodic_fd, 0, &rearm, NULL) < 0) {
        return fail("explicitly rearm canceled periodic timerfd");
    }
    struct pollfd ready = {.fd = probes->cancel_periodic_fd, .events = POLLIN};
    if (poll(&ready, 1, 2000) != 1 ||
        read(probes->cancel_periodic_fd, &expirations, sizeof(expirations)) !=
            (ssize_t)sizeof(expirations) || expirations != 1) {
        errno = EPROTO;
        return fail("deliver a new expiration after explicit rearm");
    }
    puts("timerfd cancel-on-set: no expiration after ECANCELED");
    return 0;
}

static void close_timerfd_probes(const struct timerfd_probes *probes)
{
    if (probes->relative_fd >= 0) {
        close(probes->relative_fd);
    }
    if (probes->cancel_fd >= 0) {
        close(probes->cancel_fd);
    }
    if (probes->cancel_periodic_fd >= 0) {
        close(probes->cancel_periodic_fd);
    }
}

static int prepare_posix_timer_probe(const struct timespec *original_realtime,
                                     struct posix_timer_probe *probe)
{
    sigemptyset(&probe->wait_set);
    sigaddset(&probe->wait_set, SIGRTMIN);
    if (sigprocmask(SIG_BLOCK, &probe->wait_set, &probe->old_mask) < 0) {
        return fail("block POSIX timer signal");
    }
    probe->mask_saved = 1;

    struct sigevent event = {
        .sigev_notify = SIGEV_SIGNAL,
        .sigev_signo = SIGRTMIN,
        .sigev_value.sival_int = 0x2237,
    };
    if (timer_create(CLOCK_REALTIME, &event, &probe->timer_id) < 0) {
        return fail("create periodic realtime POSIX timer");
    }
    probe->created = 1;

    struct itimerspec periodic = {
        .it_interval = {.tv_sec = 1, .tv_nsec = 0},
        .it_value = {
            .tv_sec = original_realtime->tv_sec + 1,
            .tv_nsec = original_realtime->tv_nsec,
        },
    };
    if (timer_settime(probe->timer_id, TIMER_ABSTIME, &periodic, NULL) < 0) {
        return fail("arm periodic realtime POSIX timer");
    }
    return 0;
}

static int check_posix_timer_clock_step(struct posix_timer_probe *probe)
{
    const struct timespec wait_time = {.tv_sec = 2, .tv_nsec = 0};
    siginfo_t info = {0};
    int signo = sigtimedwait(&probe->wait_set, &info, &wait_time);
    if (signo != SIGRTMIN || info.si_code != SI_TIMER ||
        info.si_value.sival_int != 0x2237 || info.si_overrun < 100) {
        fprintf(stderr,
                "POSIX timer probe: signo=%d code=%d value=%d overrun=%d\n",
                signo, info.si_code, info.si_value.sival_int,
                info.si_overrun);
        errno = EPROTO;
        return fail("merge missed periodic expirations into one signal");
    }

    struct itimerspec current = {0};
    if (timer_gettime(probe->timer_id, &current) < 0) {
        return fail("read periodic realtime POSIX timer after clock step");
    }
    int64_t remaining_nanos = timespec_to_nanos(&current.it_value);
    if (remaining_nanos <= 0 || remaining_nanos > 1000000000LL) {
        errno = EPROTO;
        return fail("advance periodic POSIX timer deadline past stepped clock");
    }

    struct itimerspec disarmed = {0};
    if (timer_settime(probe->timer_id, 0, &disarmed, NULL) < 0) {
        return fail("disarm periodic realtime POSIX timer");
    }

    const struct timespec no_wait = {0};
    errno = 0;
    if (sigtimedwait(&probe->wait_set, NULL, &no_wait) != -1 ||
        errno != EAGAIN) {
        errno = EPROTO;
        return fail("queue only one signal for missed POSIX timer periods");
    }
    return 0;
}

static void close_posix_timer_probe(struct posix_timer_probe *probe)
{
    if (probe->created) {
        timer_delete(probe->timer_id);
    }
    if (probe->mask_saved) {
        const struct timespec no_wait = {0};
        while (sigtimedwait(&probe->wait_set, NULL, &no_wait) == SIGRTMIN) {
        }
        sigprocmask(SIG_SETMASK, &probe->old_mask, NULL);
    }
}

static int restore_realtime(const struct timespec *original_realtime,
                            const struct timespec *original_monotonic)
{
    struct timespec current_monotonic = {0};
    if (clock_gettime(CLOCK_MONOTONIC, &current_monotonic) < 0) {
        return fail("read monotonic time before restoring realtime");
    }
    int64_t elapsed = timespec_to_nanos(&current_monotonic) -
                      timespec_to_nanos(original_monotonic);
    struct timespec restored = nanos_to_timespec(
        timespec_to_nanos(original_realtime) + elapsed);
    if (syscall(SYS_clock_settime, CLOCK_REALTIME, &restored) < 0) {
        return fail("restore realtime clock");
    }
    return 0;
}

int main(void)
{
    struct timespec original_realtime = {0};
    struct timespec original_monotonic = {0};
    if (clock_gettime(CLOCK_REALTIME, &original_realtime) < 0 ||
        clock_gettime(CLOCK_MONOTONIC, &original_monotonic) < 0) {
        return fail("capture original clocks");
    }

    errno = 0;
    if (expect_errno(syscall(SYS_clock_settime, CLOCK_MONOTONIC,
                             (const void *)1),
                     EINVAL, "reject non-settable clock before pointer access")) {
        return EXIT_FAILURE;
    }

    errno = 0;
    if (expect_errno(syscall(SYS_clock_settime, CLOCK_REALTIME,
                             (const void *)1),
                     EFAULT, "reject an invalid realtime pointer")) {
        return EXIT_FAILURE;
    }

    struct timespec invalid = {.tv_sec = 1, .tv_nsec = 1000000000L};
    errno = 0;
    if (expect_errno(syscall(SYS_clock_settime, CLOCK_REALTIME, &invalid),
                     EINVAL, "reject invalid nanoseconds")) {
        return EXIT_FAILURE;
    }

    struct timespec before_monotonic = {.tv_sec = 0, .tv_nsec = 0};
    errno = 0;
    if (expect_errno(syscall(SYS_clock_settime, CLOCK_REALTIME,
                             &before_monotonic),
                     EINVAL, "reject realtime before the monotonic clock")) {
        return EXIT_FAILURE;
    }

    struct timespec outside_linux_ktime = {
        .tv_sec = LINUX_TIME_SETTOD_SEC_MAX,
        .tv_nsec = 0,
    };
    errno = 0;
    if (expect_errno(syscall(SYS_clock_settime, CLOCK_REALTIME,
                             &outside_linux_ktime),
                     EINVAL, "reject realtime outside Linux ktime range")) {
        return EXIT_FAILURE;
    }

    int64_t requested_nanos = timespec_to_nanos(&original_realtime) +
                              CLOCK_STEP_SEC * 1000000000LL;
    struct timespec requested = nanos_to_timespec(requested_nanos);
    if (check_unprivileged_set_is_rejected(&requested)) {
        return EXIT_FAILURE;
    }

    struct timerfd_probes probes = {
        .relative_fd = -1, .cancel_fd = -1, .cancel_periodic_fd = -1,
    };
    if (prepare_timerfd_probes(&original_realtime, &probes)) {
        close_timerfd_probes(&probes);
        return EXIT_FAILURE;
    }

    struct posix_timer_probe posix_probe = {0};
    if (prepare_posix_timer_probe(&original_realtime, &posix_probe)) {
        close_posix_timer_probe(&posix_probe);
        close_timerfd_probes(&probes);
        return EXIT_FAILURE;
    }

    if (syscall(SYS_clock_settime, CLOCK_REALTIME, &requested) < 0) {
        close_posix_timer_probe(&posix_probe);
        close_timerfd_probes(&probes);
        return fail("set CLOCK_REALTIME as root");
    }

    int result = check_posix_timer_clock_step(&posix_probe);
    if (result == 0) {
        result = check_realtime_observers(
            requested_nanos, timespec_to_nanos(&original_monotonic));
    }
    if (result == 0) {
        result = check_timerfd_clock_step(&probes);
    }
    if (result == 0) {
        result = check_new_file_timestamp(requested_nanos);
    }
    close_posix_timer_probe(&posix_probe);
    close_timerfd_probes(&probes);
    int restore_result = restore_realtime(&original_realtime,
                                          &original_monotonic);
    if (result != 0 || restore_result != 0) {
        return EXIT_FAILURE;
    }

    puts("STARRY_CLOCK_SETTIME_PASSED");
    puts("STARRY_GROUPED_TEST_PASSED: syscall-test-clock-settime");
    return EXIT_SUCCESS;
}
