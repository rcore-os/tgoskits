// Copyright 2026 The TGOSKits Authors
//
// SPDX-License-Identifier: Apache-2.0

#ifndef TGOSKITS_AI_RTOS_DEMO_AICP_POSIX_STREAM_H
#define TGOSKITS_AI_RTOS_DEMO_AICP_POSIX_STREAM_H

/* Expose clock_gettime and CLOCK_MONOTONIC for direct header consumers. */
#ifndef _POSIX_C_SOURCE
#define _POSIX_C_SOURCE 200809L
#endif

#include "aicp_stream.h"

#include <errno.h>
#include <limits.h>
#include <poll.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#ifdef __cplusplus
extern "C" {
#endif

struct aicp_posix_stream {
    struct aicp_stream stream;
    int fd;
    uint64_t deadline_ns;
};

static inline uint64_t aicp_posix_monotonic_ns(void) {
    struct timespec now;

    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
        return 0;
    }
    return (uint64_t)now.tv_sec * 1000000000ull + (uint64_t)now.tv_nsec;
}

static inline int aicp_posix_wait_ready(
    const struct aicp_posix_stream *posix,
    short events) {
    if (posix->deadline_ns == 0) {
        return 0;
    }

    struct pollfd pollfd = {
        .fd = posix->fd,
        .events = events,
    };
    for (;;) {
        const uint64_t now_ns = aicp_posix_monotonic_ns();
        if (now_ns == 0) {
            return errno == 0 ? -EIO : -errno;
        }
        if (now_ns >= posix->deadline_ns) {
            return -ETIMEDOUT;
        }

        const uint64_t remaining_ns = posix->deadline_ns - now_ns;
        const uint64_t remaining_ms = (remaining_ns + 999999ull) / 1000000ull;
        const int timeout_ms = remaining_ms > INT_MAX ? INT_MAX : (int)remaining_ms;
        const int result = poll(&pollfd, 1, timeout_ms);
        if (result > 0) {
            return 0;
        }
        if (result == 0) {
            return -ETIMEDOUT;
        }
        if (errno != EINTR) {
            return -errno;
        }
    }
}

static inline ptrdiff_t aicp_posix_stream_read(
    void *context,
    void *buffer,
    size_t length) {
    struct aicp_posix_stream *posix = (struct aicp_posix_stream *)context;
    for (;;) {
        const int wait_result = aicp_posix_wait_ready(posix, POLLIN);
        if (wait_result != 0) {
            return wait_result;
        }
        const ssize_t result = read(posix->fd, buffer, length);
        if (result >= 0) {
            return (ptrdiff_t)result;
        }
        if (errno != EINTR) {
            return (ptrdiff_t)-errno;
        }
    }
}

static inline ptrdiff_t aicp_posix_stream_write(
    void *context,
    const void *buffer,
    size_t length) {
    struct aicp_posix_stream *posix = (struct aicp_posix_stream *)context;
    for (;;) {
        const int wait_result = aicp_posix_wait_ready(posix, POLLOUT);
        if (wait_result != 0) {
            return wait_result;
        }
#ifdef MSG_NOSIGNAL
        const ssize_t result = send(posix->fd, buffer, length, MSG_NOSIGNAL);
#else
        const ssize_t result = write(posix->fd, buffer, length);
#endif
        if (result >= 0) {
            return (ptrdiff_t)result;
        }
        if (errno != EINTR) {
            return (ptrdiff_t)-errno;
        }
    }
}

static inline void aicp_posix_stream_init(
    struct aicp_posix_stream *posix,
    int fd) {
#ifdef SO_NOSIGPIPE
    const int one = 1;

    (void)setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof(one));
#endif
    posix->fd = fd;
    posix->deadline_ns = 0;
    posix->stream.read = aicp_posix_stream_read;
    posix->stream.write = aicp_posix_stream_write;
    posix->stream.context = posix;
}

static inline void aicp_posix_stream_set_deadline_after_ms(
    struct aicp_posix_stream *posix,
    uint32_t timeout_ms) {
    const uint64_t now_ns = aicp_posix_monotonic_ns();

    posix->deadline_ns = now_ns == 0 ? 1 : now_ns + (uint64_t)timeout_ms * 1000000ull;
}

static inline int aicp_posix_read_full(
    int fd,
    void *buffer,
    size_t length) {
    struct aicp_posix_stream posix;

    aicp_posix_stream_init(&posix, fd);
    return aicp_stream_read_full(&posix.stream, buffer, length);
}

static inline int aicp_posix_write_full(
    int fd,
    const void *buffer,
    size_t length) {
    struct aicp_posix_stream posix;

    aicp_posix_stream_init(&posix, fd);
    return aicp_stream_write_full(&posix.stream, buffer, length);
}

static inline int aicp_posix_send_frame(
    int fd,
    struct aicp_header header,
    const void *payload) {
    struct aicp_posix_stream posix;

    aicp_posix_stream_init(&posix, fd);
    return aicp_stream_send_frame(&posix.stream, header, payload);
}

static inline int aicp_posix_recv_frame(
    int fd,
    struct aicp_header *header,
    void *payload,
    size_t capacity) {
    struct aicp_posix_stream posix;

    aicp_posix_stream_init(&posix, fd);
    return aicp_stream_recv_frame(&posix.stream, header, payload, capacity);
}

#ifdef __cplusplus
}
#endif

#endif
