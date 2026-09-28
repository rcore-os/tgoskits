#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

static const char chunk[128 * 1024];

static double monotonic_seconds(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
        perror("clock_gettime");
        exit(1);
    }
    return (double)now.tv_sec + (double)now.tv_nsec / 1000000000.0;
}

int main(int argc, char **argv) {
    char *end;
    unsigned long duration;
    uint64_t total = 0;
    double deadline;

    if (argc != 2) {
        fprintf(stderr, "usage: %s <duration-seconds>\n", argv[0]);
        return 2;
    }
    errno = 0;
    duration = strtoul(argv[1], &end, 10);
    if (errno != 0 || *end != '\0' || duration == 0 || duration > 3600) {
        fprintf(stderr, "invalid duration: %s\n", argv[1]);
        return 2;
    }
    deadline = monotonic_seconds() + (double)duration;
    do {
        size_t remaining = sizeof(chunk);
        const char *next = chunk;
        while (remaining != 0) {
            ssize_t written = write(STDOUT_FILENO, next, remaining);
            if (written < 0 && errno == EINTR) {
                continue;
            }
            if (written <= 0) {
                perror("write");
                return 1;
            }
            if ((uint64_t)written > UINT64_MAX - total) {
                fprintf(stderr, "byte count overflow\n");
                return 1;
            }
            total += (uint64_t)written;
            next += written;
            remaining -= (size_t)written;
        }
    } while (monotonic_seconds() < deadline);
    fprintf(stderr, "BYTES=%" PRIu64 "\n", total);
    return 0;
}
