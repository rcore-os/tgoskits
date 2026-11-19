#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <mqueue.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/inotify.h>
#include <sys/syscall.h>
#include <sys/timerfd.h>
#include <sys/uio.h>
#include <unistd.h>

#ifndef SYS_pidfd_open
#error "SYS_pidfd_open is required"
#endif

static int failures;

static void expect_errno(long result, int expected, const char *operation)
{
    if (result == -1 && errno == expected) {
        printf("PASS: %s returned %s\n", operation, strerror(expected));
        return;
    }

    fprintf(stderr,
            "FAIL: %s returned %ld errno=%d (%s), expected errno=%d (%s)\n",
            operation, result, errno, strerror(errno), expected,
            strerror(expected));
    failures++;
}

static void check_write_rejection(int fd, int expected, const char *kind)
{
    void *bad_buffer = (void *)(uintptr_t)1;
    struct iovec bad_segment = {
        .iov_base = bad_buffer,
        .iov_len = 1,
    };
    char operation[96];

    snprintf(operation, sizeof(operation), "write(%s, bad buffer)", kind);
    errno = 0;
    expect_errno(syscall(SYS_write, fd, bad_buffer, 1), expected, operation);

    snprintf(operation, sizeof(operation), "writev(%s, bad segment)", kind);
    errno = 0;
    expect_errno(syscall(SYS_writev, fd, &bad_segment, 1), expected, operation);

    snprintf(operation, sizeof(operation),
             "pwritev2(%s, bad segment, offset=-1)", kind);
    errno = 0;
    expect_errno(syscall(SYS_pwritev2, fd, &bad_segment, 1,
                         (unsigned long)-1, 0UL, 0),
                 expected, operation);
}

static void check_timerfd(void)
{
    int fd = timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC | TFD_NONBLOCK);
    if (fd < 0) {
        perror("FAIL: timerfd_create");
        failures++;
        return;
    }

    check_write_rejection(fd, EINVAL, "timerfd");
    close(fd);
}

static void check_inotify(void)
{
    int fd = inotify_init1(IN_CLOEXEC | IN_NONBLOCK);
    if (fd < 0) {
        perror("FAIL: inotify_init1");
        failures++;
        return;
    }

    check_write_rejection(fd, EBADF, "inotify");
    close(fd);
}

static void check_pidfd(void)
{
    int fd = (int)syscall(SYS_pidfd_open, getpid(), 0);
    if (fd < 0) {
        perror("FAIL: pidfd_open");
        failures++;
        return;
    }

    check_write_rejection(fd, EINVAL, "pidfd");
    close(fd);
}

static void check_mqueue(void)
{
    char name[64];
    snprintf(name, sizeof(name), "/starry-write-precedence-%ld", (long)getpid());
    mq_unlink(name);

    mqd_t queue = mq_open(name, O_CREAT | O_EXCL | O_RDONLY, 0600, NULL);
    if (queue == (mqd_t)-1) {
        perror("FAIL: mq_open(O_RDONLY)");
        failures++;
        return;
    }
    check_write_rejection((int)queue, EBADF, "mqueue O_RDONLY");
    mq_close(queue);

    queue = mq_open(name, O_RDWR);
    if (queue == (mqd_t)-1) {
        perror("FAIL: mq_open(O_RDWR)");
        failures++;
        mq_unlink(name);
        return;
    }
    check_write_rejection((int)queue, EINVAL, "mqueue O_RDWR");
    mq_close(queue);
    mq_unlink(name);
}

int main(void)
{
    check_timerfd();
    check_inotify();
    check_pidfd();
    check_mqueue();

    if (failures != 0) {
        fprintf(stderr, "STARRY_SPECIAL_FD_WRITE_PRECEDENCE_FAILED: %d failure(s)\n",
                failures);
        return 1;
    }

    puts("STARRY_SPECIAL_FD_WRITE_PRECEDENCE_PASSED");
    return 0;
}
