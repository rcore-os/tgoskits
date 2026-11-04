#define _GNU_SOURCE

#include "sound_test.h"

#include <errno.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static _Noreturn void sound_test_vfail(const char *profile, const char *fmt,
                                       va_list ap)
{
    fprintf(stderr, "VIRTIO_SOUND_%s_FAILED: ", profile);
    vfprintf(stderr, fmt, ap);
    fputc('\n', stderr);
    fflush(NULL);
    _exit(1);
}

_Noreturn void sound_test_fail(const char *profile, const char *fmt, ...)
{
    va_list ap;

    va_start(ap, fmt);
    sound_test_vfail(profile, fmt, ap);
    va_end(ap);
}

void sound_test_require(int condition, const char *profile, const char *fmt, ...)
{
    va_list ap;

    if (condition) {
        return;
    }

    va_start(ap, fmt);
    sound_test_vfail(profile, fmt, ap);
    va_end(ap);
}

int sound_test_spawn(char *const argv[], pid_t *pid)
{
    pid_t child = fork();

    if (child < 0) {
        return -1;
    }
    if (child == 0) {
        execvp(argv[0], argv);
        fprintf(stderr, "exec %s failed: %s\n", argv[0], strerror(errno));
        _exit(127);
    }

    *pid = child;
    return 0;
}

int sound_test_wait(pid_t pid, unsigned int timeout_ms)
{
    unsigned int elapsed = 0;
    int status;

    for (;;) {
        pid_t waited = waitpid(pid, &status, WNOHANG);

        if (waited == pid) {
            if (WIFEXITED(status)) {
                return WEXITSTATUS(status);
            }
            if (WIFSIGNALED(status)) {
                return 128 + WTERMSIG(status);
            }
            return 255;
        }
        if (waited < 0) {
            if (errno == EINTR) {
                continue;
            }
            return -1;
        }
        if (elapsed >= timeout_ms) {
            kill(pid, SIGKILL);
            while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {
            }
            return -2;
        }

        struct timespec delay = {.tv_sec = 0, .tv_nsec = 10 * 1000 * 1000};

        while (nanosleep(&delay, &delay) < 0 && errno == EINTR) {
        }
        elapsed += 10;
    }
}

int sound_test_run(char *const argv[], unsigned int timeout_ms)
{
    pid_t pid;

    if (sound_test_spawn(argv, &pid) < 0) {
        return -1;
    }
    return sound_test_wait(pid, timeout_ms);
}
