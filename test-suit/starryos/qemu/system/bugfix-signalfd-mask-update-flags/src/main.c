#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/signalfd.h>
#include <unistd.h>

static int passed;
static int failed;

static void expect_true(int condition, const char *name)
{
    if (condition) {
        printf("PASS: %s\n", name);
        passed++;
        return;
    }
    printf("FAIL: %s: errno=%d (%s)\n", name, errno, strerror(errno));
    failed++;
}

static void add_signal(sigset_t *mask, int signal_number)
{
    expect_true(sigaddset(mask, signal_number) == 0, "add signal to mask");
}

int main(void)
{
    printf("=== bugfix-signalfd-mask-update-flags ===\n");

    sigset_t initial_mask;
    sigemptyset(&initial_mask);
    add_signal(&initial_mask, SIGUSR1);

    int signal_fd = signalfd(-1, &initial_mask, 0);
    expect_true(signal_fd >= 0, "create blocking signalfd without CLOEXEC");

    if (signal_fd >= 0) {
        sigset_t expanded_mask = initial_mask;
        add_signal(&expanded_mask, SIGUSR2);

        errno = 0;
        int result = signalfd(signal_fd, &expanded_mask,
                              SFD_CLOEXEC | SFD_NONBLOCK);
        expect_true(result == signal_fd,
                    "update existing signalfd with valid flags returns same fd");

        int descriptor_flags = fcntl(signal_fd, F_GETFD);
        expect_true(descriptor_flags >= 0 &&
                        (descriptor_flags & FD_CLOEXEC) == 0,
                    "update does not add FD_CLOEXEC");

        int status_flags = fcntl(signal_fd, F_GETFL);
        expect_true(status_flags >= 0 &&
                        (status_flags & O_NONBLOCK) == 0,
                    "update does not add O_NONBLOCK");

        close(signal_fd);
    }

    sigemptyset(&initial_mask);
    add_signal(&initial_mask, SIGUSR1);
    signal_fd =
        signalfd(-1, &initial_mask, SFD_CLOEXEC | SFD_NONBLOCK);
    expect_true(signal_fd >= 0,
                "create nonblocking CLOEXEC signalfd");

    if (signal_fd >= 0) {
        sigset_t expanded_mask = initial_mask;
        add_signal(&expanded_mask, SIGUSR2);

        errno = 0;
        int result = signalfd(signal_fd, &expanded_mask,
                              SFD_CLOEXEC | SFD_NONBLOCK);
        expect_true(result == signal_fd,
                    "systemd-style repeated flags update returns same fd");

        close(signal_fd);
    }

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("STARRY_SIGNALFD_MASK_UPDATE_FLAGS_PASSED\n");
        printf("STARRY_GROUPED_TEST_PASSED: bugfix-signalfd-mask-update-flags\n");
        return EXIT_SUCCESS;
    }
    printf("STARRY_GROUPED_TEST_FAILED: bugfix-signalfd-mask-update-flags\n");
    return EXIT_FAILURE;
}
