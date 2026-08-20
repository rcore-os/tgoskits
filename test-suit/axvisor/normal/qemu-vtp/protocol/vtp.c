/*
 * VTP - Virtual Transport Protocol
 *
 * Implementation of the shared codec. See vtp.h for the wire format and
 * docs/design/axvisor-vtp.md for the protocol definition.
 *
 * The codec is allocation-free: callers provide output buffers and receive
 * pointers into the inbound datagram.
 */

#include "vtp.h"

#include <string.h>

/* ------------------------------------------------------------------ */
/* Byte order helpers (manual big-endian packing, no htons dependency) */
/* ------------------------------------------------------------------ */

static uint16_t be16_get(const uint8_t *p)
{
    return (uint16_t)(((uint16_t)p[0] << 8) | (uint16_t)p[1]);
}

static uint32_t be32_get(const uint8_t *p)
{
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) |
           ((uint32_t)p[2] << 8) | (uint32_t)p[3];
}

static void be16_put(uint8_t *p, uint16_t v)
{
    p[0] = (uint8_t)(v >> 8);
    p[1] = (uint8_t)(v & 0xFFu);
}

static void be32_put(uint8_t *p, uint32_t v)
{
    p[0] = (uint8_t)(v >> 24);
    p[1] = (uint8_t)((v >> 16) & 0xFFu);
    p[2] = (uint8_t)((v >> 8) & 0xFFu);
    p[3] = (uint8_t)(v & 0xFFu);
}

/* ------------------------------------------------------------------ */
/* CRC16-CCITT (poly 0x1021, init 0xFFFF, no reflection, no xorout)   */
/* ------------------------------------------------------------------ */

uint16_t vtp_crc16(const uint8_t *data, size_t len)
{
    return vtp_crc16_update(0xFFFFu, data, len);
}

uint16_t vtp_crc16_update(uint16_t crc, const uint8_t *data, size_t len)
{
    size_t i;
    int bit;

    for (i = 0; i < len; i++) {
        crc ^= (uint16_t)data[i] << 8;
        for (bit = 0; bit < 8; bit++) {
            if (crc & 0x8000u) {
                crc = (uint16_t)((crc << 1) ^ 0x1021u);
            } else {
                crc = (uint16_t)(crc << 1);
            }
        }
    }
    return crc;
}

/* ------------------------------------------------------------------ */
/* Peer state                                                         */
/* ------------------------------------------------------------------ */

void vtp_peer_init(vtp_peer_t *peer, uint32_t first_tx_seq)
{
    peer->next_tx_seq = first_tx_seq;
    peer->last_rx_seq = 0;
    peer->rx_initialized = 0;
}

uint32_t vtp_tx_seq(vtp_peer_t *peer)
{
    return peer->next_tx_seq++;
}

int vtp_rx_accept(vtp_peer_t *peer, uint32_t seq)
{
    if (!peer->rx_initialized) {
        peer->last_rx_seq = seq;
        peer->rx_initialized = 1;
        return VTP_ERR_OK;
    }
    /* Accept any seq that is not the one we already consumed. A reordered or
     * duplicated datagram with an older seq is a duplicate from the app's
     * point of view. */
    if (seq == peer->last_rx_seq) {
        return -VTP_ERR_SEQ_MISMATCH;
    }
    peer->last_rx_seq = seq;
    return VTP_ERR_OK;
}

/* ------------------------------------------------------------------ */
/* Core encode / decode                                               */
/* ------------------------------------------------------------------ */

int vtp_encode(uint8_t *out, size_t cap, uint8_t msg_type, uint8_t flags,
               uint32_t seq, uint32_t timestamp_ms, const uint8_t *payload,
               uint16_t payload_len)
{
    size_t total;

    if (out == NULL || cap < VTP_HEADER_LEN) {
        return -VTP_ERR_INVALID_PAYLOAD;
    }
    if (payload_len > VTP_MAX_PAYLOAD || (payload == NULL && payload_len > 0)) {
        return -VTP_ERR_INVALID_PAYLOAD;
    }

    total = VTP_HEADER_LEN + (size_t)payload_len;
    if (cap < total) {
        return -VTP_ERR_INVALID_PAYLOAD;
    }

    be16_put(out + 0, VTP_MAGIC);
    out[2] = VTP_VERSION;
    out[3] = msg_type;
    out[4] = flags;
    out[5] = 0; /* reserved */
    be32_put(out + 6, seq);
    be32_put(out + 10, timestamp_ms);
    be16_put(out + 14, payload_len);

    /* checksum covers the fixed header (bytes 0..15) + payload; the checksum
     * field itself (bytes 16..17) is excluded. */
    if (payload_len > 0) {
        memcpy(out + VTP_HEADER_LEN, payload, payload_len);
    }
    {
        uint16_t crc = vtp_crc16(out, 16);
        if (payload_len > 0) {
            crc = vtp_crc16_update(crc, out + VTP_HEADER_LEN, payload_len);
        }
        be16_put(out + 16, crc);
    }

    return (int)total;
}

int vtp_decode(const uint8_t *buf, size_t len, vtp_header_t *hdr,
               const uint8_t **out_payload, uint16_t *out_payload_len)
{
    uint16_t expected_crc;
    uint16_t payload_len;

    if (buf == NULL || hdr == NULL || out_payload == NULL || out_payload_len == NULL) {
        return -VTP_ERR_INVALID_PAYLOAD;
    }
    if (len < VTP_HEADER_LEN) {
        return -VTP_ERR_INVALID_PAYLOAD;
    }

    hdr->magic = be16_get(buf + 0);
    hdr->version = buf[2];
    hdr->msg_type = buf[3];
    hdr->flags = buf[4];
    hdr->reserved = buf[5];
    hdr->seq = be32_get(buf + 6);
    hdr->timestamp_ms = be32_get(buf + 10);
    hdr->payload_len = be16_get(buf + 14);
    hdr->checksum = be16_get(buf + 16);

    if (hdr->magic != VTP_MAGIC) {
        return -VTP_ERR_BAD_MAGIC;
    }
    if (hdr->version != VTP_VERSION) {
        return -VTP_ERR_UNSUPPORTED_VERSION;
    }
    if (hdr->reserved != 0) {
        return -VTP_ERR_INVALID_PAYLOAD;
    }
    payload_len = hdr->payload_len;
    if (payload_len > VTP_MAX_PAYLOAD) {
        return -VTP_ERR_INVALID_PAYLOAD;
    }
    if ((size_t)payload_len > len - VTP_HEADER_LEN) {
        return -VTP_ERR_INVALID_PAYLOAD;
    }

    expected_crc = vtp_crc16(buf, 16);
    if (payload_len > 0) {
        expected_crc = vtp_crc16_update(expected_crc, buf + VTP_HEADER_LEN,
                                        payload_len);
    }
    if (expected_crc != hdr->checksum) {
        return -VTP_ERR_BAD_CHECKSUM;
    }

    *out_payload = buf + VTP_HEADER_LEN;
    *out_payload_len = payload_len;
    return VTP_ERR_OK;
}

/* ------------------------------------------------------------------ */
/* Typed message builders                                             */
/* ------------------------------------------------------------------ */

int vtp_encode_control(uint8_t *out, size_t cap, uint8_t flags, uint32_t seq,
                       uint32_t timestamp_ms, uint8_t cmd, const uint8_t *data,
                       uint8_t data_len)
{
    uint8_t payload[1 + 255];
    size_t plen;

    payload[0] = cmd;
    if (data_len > 0) {
        memcpy(payload + 1, data, data_len);
    }
    plen = 1 + (size_t)data_len;
    return vtp_encode(out, cap, VTP_MSG_CONTROL, flags, seq, timestamp_ms,
                      payload, (uint16_t)plen);
}

int vtp_encode_status(uint8_t *out, size_t cap, uint8_t flags, uint32_t seq,
                      uint32_t timestamp_ms, uint8_t state, uint8_t code,
                      uint32_t uptime_ms, const uint8_t *extra,
                      uint8_t extra_len)
{
    uint8_t payload[2 + 4 + 255];
    size_t plen;

    payload[0] = state;
    payload[1] = code;
    be32_put(payload + 2, uptime_ms);
    if (extra_len > 0) {
        memcpy(payload + 6, extra, extra_len);
    }
    plen = 6 + (size_t)extra_len;
    return vtp_encode(out, cap, VTP_MSG_STATUS, flags, seq, timestamp_ms,
                      payload, (uint16_t)plen);
}

int vtp_encode_error(uint8_t *out, size_t cap, uint8_t flags, uint32_t seq,
                     uint32_t timestamp_ms, uint16_t error_code, uint8_t source,
                     const uint8_t *detail, uint8_t detail_len)
{
    uint8_t payload[2 + 1 + 255];
    size_t plen;

    be16_put(payload + 0, error_code);
    payload[2] = source;
    if (detail_len > 0) {
        memcpy(payload + 3, detail, detail_len);
    }
    plen = 3 + (size_t)detail_len;
    return vtp_encode(out, cap, VTP_MSG_ERROR, flags, seq, timestamp_ms, payload,
                      (uint16_t)plen);
}

int vtp_encode_ack(uint8_t *out, size_t cap, uint32_t seq,
                   uint32_t timestamp_ms, uint8_t ack, uint16_t error_code)
{
    uint8_t payload[1 + 2];

    payload[0] = ack;
    be16_put(payload + 1, error_code);
    return vtp_encode(out, cap, VTP_MSG_ACK, VTP_FLAG_RESPONSE, seq,
                      timestamp_ms, payload, sizeof(payload));
}

int vtp_encode_data(uint8_t *out, size_t cap, uint8_t flags, uint32_t seq,
                    uint32_t timestamp_ms, const uint8_t *data, uint16_t len)
{
    return vtp_encode(out, cap, VTP_MSG_DATA, flags, seq, timestamp_ms, data, len);
}

/* ------------------------------------------------------------------ */
/* Typed message parsers                                              */
/* ------------------------------------------------------------------ */

int vtp_parse_control(const uint8_t *payload, uint16_t len, uint8_t *cmd,
                      const uint8_t **data, uint8_t *data_len)
{
    if (payload == NULL || cmd == NULL || data == NULL || data_len == NULL || len < 1) {
        return -VTP_ERR_INVALID_PAYLOAD;
    }
    *cmd = payload[0];
    *data = payload + 1;
    *data_len = (uint8_t)(len - 1);
    return VTP_ERR_OK;
}

int vtp_parse_status(const uint8_t *payload, uint16_t len, uint8_t *state,
                     uint8_t *code, uint32_t *uptime_ms, const uint8_t **extra,
                     uint8_t *extra_len)
{
    if (payload == NULL || state == NULL || code == NULL || uptime_ms == NULL ||
        extra == NULL || extra_len == NULL || len < 6) {
        return -VTP_ERR_INVALID_PAYLOAD;
    }
    *state = payload[0];
    *code = payload[1];
    *uptime_ms = be32_get(payload + 2);
    *extra = payload + 6;
    *extra_len = (uint8_t)(len - 6);
    return VTP_ERR_OK;
}

int vtp_parse_error(const uint8_t *payload, uint16_t len, uint16_t *error_code,
                    uint8_t *source, const uint8_t **detail, uint8_t *detail_len)
{
    if (payload == NULL || error_code == NULL || source == NULL || detail == NULL ||
        detail_len == NULL || len < 3) {
        return -VTP_ERR_INVALID_PAYLOAD;
    }
    *error_code = be16_get(payload + 0);
    *source = payload[2];
    *detail = payload + 3;
    *detail_len = (uint8_t)(len - 3);
    return VTP_ERR_OK;
}

int vtp_parse_ack(const uint8_t *payload, uint16_t len, uint8_t *ack,
                  uint16_t *error_code)
{
    if (payload == NULL || ack == NULL || error_code == NULL || len < 3) {
        return -VTP_ERR_INVALID_PAYLOAD;
    }
    *ack = payload[0];
    *error_code = be16_get(payload + 1);
    return VTP_ERR_OK;
}
