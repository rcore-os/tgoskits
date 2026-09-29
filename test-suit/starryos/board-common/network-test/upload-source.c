#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

static const char chunk[128 * 1024];

static struct timespec monotonic_now(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
        perror("clock_gettime");
        exit(1);
    }
    return now;
}

static int timespec_before(struct timespec left, struct timespec right) {
    return left.tv_sec < right.tv_sec ||
           (left.tv_sec == right.tv_sec && left.tv_nsec < right.tv_nsec);
}

static uint64_t elapsed_milliseconds(struct timespec start,
                                     struct timespec end) {
    uint64_t seconds = (uint64_t)(end.tv_sec - start.tv_sec);
    long nanoseconds = end.tv_nsec - start.tv_nsec;
    if (nanoseconds < 0) {
        seconds--;
        nanoseconds += 1000000000L;
    }
    return seconds * 1000 + (uint64_t)nanoseconds / 1000000;
}

int main(int argc, char **argv) {
    char *end;
    unsigned long duration;
    uint64_t total = 0;
    struct timespec started;
    struct timespec deadline;
    struct timespec now;

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
    started = monotonic_now();
    deadline = started;
    deadline.tv_sec += (time_t)duration;
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
        now = monotonic_now();
    } while (timespec_before(now, deadline));
    fprintf(stderr, "BYTES=%" PRIu64 "\nELAPSED_MS=%" PRIu64 "\n", total,
            elapsed_milliseconds(started, now));
    return 0;
}
