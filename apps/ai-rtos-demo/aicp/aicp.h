// Copyright 2026 The TGOSKits Authors
//
// SPDX-License-Identifier: Apache-2.0

#ifndef TGOSKITS_AI_RTOS_DEMO_AICP_H
#define TGOSKITS_AI_RTOS_DEMO_AICP_H

#include <float.h>
#include <math.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#ifdef __cplusplus
extern "C" {
#endif

#define AICP_MAGIC 0xA1C0u
#define AICP_VERSION 1u
#define AICP_HEADER_LEN 32u
#define AICP_MAX_PAYLOAD 4096u
#define AICP_CONTROL_PAYLOAD_LEN 24u
#define AICP_STATUS_PAYLOAD_LEN 24u

enum aicp_msg_type {
    AICP_MSG_HELLO = 0x01,
    AICP_MSG_CONTROL_SET = 0x02,
    AICP_MSG_STATUS = 0x03,
    AICP_MSG_ERROR = 0x04,
    AICP_MSG_HEARTBEAT = 0x05,
};

enum aicp_error_code {
    AICP_OK = 0,
    AICP_ERR_VERSION = 1,
    AICP_ERR_CRC = 2,
    AICP_ERR_BAD_TYPE = 3,
    AICP_ERR_BAD_PAYLOAD = 4,
    AICP_ERR_TIMEOUT = 5,
    AICP_ERR_INTERNAL = 6,
    AICP_ERR_SEQUENCE = 7,
};

struct aicp_header {
    uint16_t magic;
    uint8_t version;
    uint8_t msg_type;
    uint16_t flags;
    uint16_t header_len;
    uint32_t payload_len;
    uint32_t seq;
    uint64_t timestamp_ns;
    uint16_t error_code;
    uint16_t crc16;
    uint32_t reserved;
};

struct aicp_control_payload {
    float target;
    float kp;
    float ki;
    float kd;
    float feed_forward;
    uint32_t mode;
};

struct aicp_status_payload {
    float setpoint;
    float measured;
    float control_output;
    float error;
    uint32_t mode;
    uint32_t applied_seq;
};

static inline int aicp_control_payload_is_valid(const struct aicp_control_payload *payload) {
    return payload != NULL && isfinite(payload->target) && isfinite(payload->kp) &&
           isfinite(payload->ki) && isfinite(payload->kd) && isfinite(payload->feed_forward) &&
           payload->target >= -1.0f && payload->target <= 1.0f &&
           payload->kp >= 0.0f && payload->kp <= 10.0f &&
           payload->ki >= 0.0f && payload->ki <= 10.0f &&
           payload->kd >= 0.0f && payload->kd <= 10.0f &&
           payload->feed_forward >= -1.0f && payload->feed_forward <= 1.0f &&
           payload->mode <= 1;
}

static inline int aicp_header_options_are_supported(const struct aicp_header *header) {
    return header != NULL && header->flags == 0 && header->reserved == 0;
}

_Static_assert(
    sizeof(float) == sizeof(uint32_t) && FLT_RADIX == 2 && FLT_MANT_DIG == 24,
    "AICP payloads require IEEE-754 binary32 floats");

static inline uint16_t aicp_bswap16(uint16_t v) {
    return (uint16_t)((v << 8) | (v >> 8));
}

static inline uint32_t aicp_bswap32(uint32_t v) {
    return ((v & 0x000000ffu) << 24) | ((v & 0x0000ff00u) << 8) |
           ((v & 0x00ff0000u) >> 8) | ((v & 0xff000000u) >> 24);
}

static inline uint64_t aicp_bswap64(uint64_t v) {
    return ((uint64_t)aicp_bswap32((uint32_t)v) << 32) |
           (uint64_t)aicp_bswap32((uint32_t)(v >> 32));
}

#if defined(__BYTE_ORDER__) && __BYTE_ORDER__ == __ORDER_BIG_ENDIAN__
static inline uint16_t aicp_htobe16(uint16_t v) { return v; }
static inline uint32_t aicp_htobe32(uint32_t v) { return v; }
static inline uint64_t aicp_htobe64(uint64_t v) { return v; }
#else
static inline uint16_t aicp_htobe16(uint16_t v) { return aicp_bswap16(v); }
static inline uint32_t aicp_htobe32(uint32_t v) { return aicp_bswap32(v); }
static inline uint64_t aicp_htobe64(uint64_t v) { return aicp_bswap64(v); }
#endif

static inline uint16_t aicp_be16toh(uint16_t v) { return aicp_htobe16(v); }
static inline uint32_t aicp_be32toh(uint32_t v) { return aicp_htobe32(v); }
static inline uint64_t aicp_be64toh(uint64_t v) { return aicp_htobe64(v); }

static inline void aicp_wire_encode_u32(uint8_t out[sizeof(uint32_t)], uint32_t value) {
    const uint32_t wire = aicp_htobe32(value);

    memcpy(out, &wire, sizeof(wire));
}

static inline uint32_t aicp_wire_decode_u32(const uint8_t in[sizeof(uint32_t)]) {
    uint32_t wire;

    memcpy(&wire, in, sizeof(wire));
    return aicp_be32toh(wire);
}

static inline void aicp_wire_encode_f32(uint8_t out[sizeof(uint32_t)], float value) {
    uint32_t bits;

    memcpy(&bits, &value, sizeof(bits));
    aicp_wire_encode_u32(out, bits);
}

static inline float aicp_wire_decode_f32(const uint8_t in[sizeof(uint32_t)]) {
    const uint32_t bits = aicp_wire_decode_u32(in);
    float value;

    memcpy(&value, &bits, sizeof(value));
    return value;
}

/*
 * AICP control and status payloads are fixed 24-byte wire records. Each
 * float is IEEE-754 binary32 and every scalar is encoded in network byte
 * order; the C struct layout is never sent directly.
 */
static inline void aicp_control_payload_encode(
    const struct aicp_control_payload *payload,
    uint8_t out[AICP_CONTROL_PAYLOAD_LEN]) {
    aicp_wire_encode_f32(out + 0, payload->target);
    aicp_wire_encode_f32(out + 4, payload->kp);
    aicp_wire_encode_f32(out + 8, payload->ki);
    aicp_wire_encode_f32(out + 12, payload->kd);
    aicp_wire_encode_f32(out + 16, payload->feed_forward);
    aicp_wire_encode_u32(out + 20, payload->mode);
}

static inline void aicp_control_payload_decode(
    const uint8_t in[AICP_CONTROL_PAYLOAD_LEN],
    struct aicp_control_payload *payload) {
    payload->target = aicp_wire_decode_f32(in + 0);
    payload->kp = aicp_wire_decode_f32(in + 4);
    payload->ki = aicp_wire_decode_f32(in + 8);
    payload->kd = aicp_wire_decode_f32(in + 12);
    payload->feed_forward = aicp_wire_decode_f32(in + 16);
    payload->mode = aicp_wire_decode_u32(in + 20);
}

static inline void aicp_status_payload_encode(
    const struct aicp_status_payload *payload,
    uint8_t out[AICP_STATUS_PAYLOAD_LEN]) {
    aicp_wire_encode_f32(out + 0, payload->setpoint);
    aicp_wire_encode_f32(out + 4, payload->measured);
    aicp_wire_encode_f32(out + 8, payload->control_output);
    aicp_wire_encode_f32(out + 12, payload->error);
    aicp_wire_encode_u32(out + 16, payload->mode);
    aicp_wire_encode_u32(out + 20, payload->applied_seq);
}

static inline void aicp_status_payload_decode(
    const uint8_t in[AICP_STATUS_PAYLOAD_LEN],
    struct aicp_status_payload *payload) {
    payload->setpoint = aicp_wire_decode_f32(in + 0);
    payload->measured = aicp_wire_decode_f32(in + 4);
    payload->control_output = aicp_wire_decode_f32(in + 8);
    payload->error = aicp_wire_decode_f32(in + 12);
    payload->mode = aicp_wire_decode_u32(in + 16);
    payload->applied_seq = aicp_wire_decode_u32(in + 20);
}

static inline uint16_t aicp_crc16_ccitt_update(uint16_t crc, const void *data, size_t len) {
    const uint8_t *p = (const uint8_t *)data;

    for (size_t i = 0; i < len; i++) {
        crc ^= (uint16_t)p[i] << 8;
        for (unsigned bit = 0; bit < 8; bit++) {
            crc = (crc & 0x8000u) ? (uint16_t)((crc << 1) ^ 0x1021u)
                                  : (uint16_t)(crc << 1);
        }
    }
    return crc;
}

static inline uint16_t aicp_crc16_ccitt(const void *data, size_t len) {
    return aicp_crc16_ccitt_update(0xffffu, data, len);
}

static inline void aicp_header_encode(const struct aicp_header *hdr, uint8_t out[AICP_HEADER_LEN]);
static inline void aicp_header_decode(const uint8_t in[AICP_HEADER_LEN], struct aicp_header *hdr);

static inline uint16_t aicp_frame_crc(struct aicp_header hdr, const void *payload) {
    uint8_t wire[AICP_HEADER_LEN];
    size_t payload_len = hdr.payload_len;

    hdr.crc16 = 0;
    aicp_header_encode(&hdr, wire);

    uint16_t crc = aicp_crc16_ccitt(wire, sizeof(wire));
    if (payload_len != 0 && payload != NULL) {
        crc = aicp_crc16_ccitt_update(crc, payload, payload_len);
    }
    return crc;
}

static inline void aicp_header_encode(const struct aicp_header *hdr, uint8_t out[AICP_HEADER_LEN]) {
    uint16_t v16;
    uint32_t v32;
    uint64_t v64;

    v16 = aicp_htobe16(hdr->magic);
    memcpy(out + 0, &v16, sizeof(v16));
    out[2] = hdr->version;
    out[3] = hdr->msg_type;
    v16 = aicp_htobe16(hdr->flags);
    memcpy(out + 4, &v16, sizeof(v16));
    v16 = aicp_htobe16(hdr->header_len);
    memcpy(out + 6, &v16, sizeof(v16));
    v32 = aicp_htobe32(hdr->payload_len);
    memcpy(out + 8, &v32, sizeof(v32));
    v32 = aicp_htobe32(hdr->seq);
    memcpy(out + 12, &v32, sizeof(v32));
    v64 = aicp_htobe64(hdr->timestamp_ns);
    memcpy(out + 16, &v64, sizeof(v64));
    v16 = aicp_htobe16(hdr->error_code);
    memcpy(out + 24, &v16, sizeof(v16));
    v16 = aicp_htobe16(hdr->crc16);
    memcpy(out + 26, &v16, sizeof(v16));
    v32 = aicp_htobe32(hdr->reserved);
    memcpy(out + 28, &v32, sizeof(v32));
}

static inline void aicp_header_decode(const uint8_t in[AICP_HEADER_LEN], struct aicp_header *hdr) {
    uint16_t v16;
    uint32_t v32;
    uint64_t v64;

    memcpy(&v16, in + 0, sizeof(v16));
    hdr->magic = aicp_be16toh(v16);
    hdr->version = in[2];
    hdr->msg_type = in[3];
    memcpy(&v16, in + 4, sizeof(v16));
    hdr->flags = aicp_be16toh(v16);
    memcpy(&v16, in + 6, sizeof(v16));
    hdr->header_len = aicp_be16toh(v16);
    memcpy(&v32, in + 8, sizeof(v32));
    hdr->payload_len = aicp_be32toh(v32);
    memcpy(&v32, in + 12, sizeof(v32));
    hdr->seq = aicp_be32toh(v32);
    memcpy(&v64, in + 16, sizeof(v64));
    hdr->timestamp_ns = aicp_be64toh(v64);
    memcpy(&v16, in + 24, sizeof(v16));
    hdr->error_code = aicp_be16toh(v16);
    memcpy(&v16, in + 26, sizeof(v16));
    hdr->crc16 = aicp_be16toh(v16);
    memcpy(&v32, in + 28, sizeof(v32));
    hdr->reserved = aicp_be32toh(v32);
}

static inline struct aicp_header aicp_make_header(
    uint8_t msg_type,
    uint16_t flags,
    uint32_t payload_len,
    uint32_t seq,
    uint64_t timestamp_ns,
    uint16_t error_code) {
    struct aicp_header hdr;
    memset(&hdr, 0, sizeof(hdr));
    hdr.magic = AICP_MAGIC;
    hdr.version = AICP_VERSION;
    hdr.msg_type = msg_type;
    hdr.flags = flags;
    hdr.header_len = AICP_HEADER_LEN;
    hdr.payload_len = payload_len;
    hdr.seq = seq;
    hdr.timestamp_ns = timestamp_ns;
    hdr.error_code = error_code;
    return hdr;
}

#ifdef __cplusplus
}
#endif

#endif
