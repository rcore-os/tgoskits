// Copyright 2026 The TGOSKits Authors
//
// SPDX-License-Identifier: Apache-2.0

#ifndef TGOSKITS_AI_RTOS_DEMO_AICP_DATAGRAM_H
#define TGOSKITS_AI_RTOS_DEMO_AICP_DATAGRAM_H

#include "aicp.h"

#include <errno.h>
#include <stddef.h>
#include <string.h>

#ifdef __cplusplus
extern "C" {
#endif

static inline int aicp_datagram_encode(
    struct aicp_header header,
    const void *payload,
    void *packet,
    size_t capacity,
    size_t *packet_len) {
    if (packet == NULL || packet_len == NULL ||
        (header.payload_len != 0 && payload == NULL)) {
        return -EINVAL;
    }
    if (header.payload_len > AICP_MAX_PAYLOAD) {
        return -EMSGSIZE;
    }
    if (!aicp_header_options_are_supported(&header)) {
        return -EINVAL;
    }

    const size_t required = AICP_HEADER_LEN + (size_t)header.payload_len;
    if (capacity < required) {
        return -ENOBUFS;
    }

    uint8_t *wire = (uint8_t *)packet;
    header.magic = AICP_MAGIC;
    header.version = AICP_VERSION;
    header.header_len = AICP_HEADER_LEN;
    header.crc16 = aicp_frame_crc(header, payload);
    aicp_header_encode(&header, wire);
    if (header.payload_len != 0) {
        memcpy(wire + AICP_HEADER_LEN, payload, header.payload_len);
    }
    *packet_len = required;
    return 0;
}

static inline int aicp_datagram_decode(
    const void *packet,
    size_t packet_len,
    struct aicp_header *header,
    void *payload,
    size_t capacity) {
    if (packet == NULL || header == NULL ||
        (capacity != 0 && payload == NULL)) {
        return -EINVAL;
    }
    if (packet_len < AICP_HEADER_LEN) {
        return -EMSGSIZE;
    }

    const uint8_t *wire = (const uint8_t *)packet;
    aicp_header_decode(wire, header);
    if (header->magic != AICP_MAGIC || header->version != AICP_VERSION ||
        header->header_len != AICP_HEADER_LEN ||
        !aicp_header_options_are_supported(header)) {
        return -EPROTO;
    }
    if (header->payload_len > AICP_MAX_PAYLOAD ||
        header->payload_len > capacity) {
        return -EMSGSIZE;
    }

    const size_t expected = AICP_HEADER_LEN + (size_t)header->payload_len;
    if (packet_len != expected) {
        return -EBADMSG;
    }
    if (header->payload_len != 0) {
        memcpy(payload, wire + AICP_HEADER_LEN, header->payload_len);
    }

    const uint16_t expected_crc = header->crc16;
    const uint16_t actual_crc = aicp_frame_crc(*header, payload);
    return expected_crc == actual_crc ? 0 : -EBADMSG;
}

#ifdef __cplusplus
}
#endif

#endif
