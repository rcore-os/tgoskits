// Copyright 2026 The TGOSKits Authors
//
// SPDX-License-Identifier: Apache-2.0

#ifndef TGOSKITS_AI_RTOS_DEMO_AICP_STREAM_H
#define TGOSKITS_AI_RTOS_DEMO_AICP_STREAM_H

#include "aicp.h"

#include <errno.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef ptrdiff_t (*aicp_stream_read_fn)(
    void *context,
    void *buffer,
    size_t length);
typedef ptrdiff_t (*aicp_stream_write_fn)(
    void *context,
    const void *buffer,
    size_t length);

struct aicp_stream {
    aicp_stream_read_fn read;
    aicp_stream_write_fn write;
    void *context;
};

static inline int aicp_stream_read_full(
    struct aicp_stream *stream,
    void *buffer,
    size_t length) {
    if (stream == NULL || stream->read == NULL ||
        (length != 0 && buffer == NULL)) {
        return -EINVAL;
    }

    uint8_t *cursor = (uint8_t *)buffer;
    while (length != 0) {
        const ptrdiff_t received = stream->read(stream->context, cursor, length);
        if (received == 0) {
            return -ECONNRESET;
        }
        if (received < 0) {
            return (int)received;
        }
        if ((size_t)received > length) {
            return -EIO;
        }
        cursor += (size_t)received;
        length -= (size_t)received;
    }
    return 0;
}

static inline int aicp_stream_write_full(
    struct aicp_stream *stream,
    const void *buffer,
    size_t length) {
    if (stream == NULL || stream->write == NULL ||
        (length != 0 && buffer == NULL)) {
        return -EINVAL;
    }

    const uint8_t *cursor = (const uint8_t *)buffer;
    while (length != 0) {
        const ptrdiff_t sent = stream->write(stream->context, cursor, length);
        if (sent == 0) {
            return -ECONNRESET;
        }
        if (sent < 0) {
            return (int)sent;
        }
        if ((size_t)sent > length) {
            return -EIO;
        }
        cursor += (size_t)sent;
        length -= (size_t)sent;
    }
    return 0;
}

static inline int aicp_stream_send_frame(
    struct aicp_stream *stream,
    struct aicp_header header,
    const void *payload) {
    if (header.payload_len > AICP_MAX_PAYLOAD) {
        return -EMSGSIZE;
    }
    if (!aicp_header_options_are_supported(&header)) {
        return -EINVAL;
    }
    if (header.payload_len != 0 && payload == NULL) {
        return -EINVAL;
    }

    uint8_t wire[AICP_HEADER_LEN];
    header.magic = AICP_MAGIC;
    header.version = AICP_VERSION;
    header.header_len = AICP_HEADER_LEN;
    header.crc16 = aicp_frame_crc(header, payload);
    aicp_header_encode(&header, wire);

    int result = aicp_stream_write_full(stream, wire, sizeof(wire));
    if (result != 0) {
        return result;
    }
    if (header.payload_len != 0) {
        return aicp_stream_write_full(stream, payload, header.payload_len);
    }
    return 0;
}

static inline int aicp_stream_recv_frame(
    struct aicp_stream *stream,
    struct aicp_header *header,
    void *payload,
    size_t capacity) {
    if (header == NULL || (capacity != 0 && payload == NULL)) {
        return -EINVAL;
    }

    uint8_t wire[AICP_HEADER_LEN];
    int result = aicp_stream_read_full(stream, wire, sizeof(wire));
    if (result != 0) {
        return result;
    }

    aicp_header_decode(wire, header);
    if (header->magic != AICP_MAGIC || header->header_len != AICP_HEADER_LEN ||
        !aicp_header_options_are_supported(header)) {
        return -EPROTO;
    }
    if (header->payload_len > capacity || header->payload_len > AICP_MAX_PAYLOAD) {
        return -EMSGSIZE;
    }
    if (header->payload_len != 0) {
        result = aicp_stream_read_full(stream, payload, header->payload_len);
        if (result != 0) {
            return result;
        }
    }

    const uint16_t expected_crc = header->crc16;
    const uint16_t actual_crc = aicp_frame_crc(*header, payload);
    if (expected_crc != actual_crc) {
        return -EBADMSG;
    }
    return 0;
}

#ifdef __cplusplus
}
#endif

#endif
