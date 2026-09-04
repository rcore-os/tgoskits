/* icpc wire format — must match components/icpc/src/header.rs CRC + layout. */
#include "icpc-wire.h"

#include <string.h>

static uint32_t crc_update_byte(uint32_t crc, uint8_t byte)
{
    const uint32_t poly = 0xEDB88320u;
    crc ^= (uint32_t)byte;
    for (int i = 0; i < 8; i++) {
        uint32_t mask = (uint32_t)(-(int32_t)(crc & 1u));
        crc = (crc >> 1) ^ (poly & mask);
    }
    return crc;
}

uint32_t icpc_crc32(const uint8_t *data, size_t len)
{
    uint32_t crc = 0xFFFFFFFFu;
    for (size_t i = 0; i < len; i++)
        crc = crc_update_byte(crc, data[i]);
    return ~crc;
}

static uint32_t frame_crc(const uint8_t *header, const uint8_t *payload,
                          size_t payload_len)
{
    uint32_t crc = 0xFFFFFFFFu;
    for (size_t i = 0; i < ICPC_HEADER_LEN; i++) {
        uint8_t b = header[i];
        if (i >= 20 && i < 24)
            b = 0;
        crc = crc_update_byte(crc, b);
    }
    for (size_t i = 0; i < payload_len; i++)
        crc = crc_update_byte(crc, payload[i]);
    return ~crc;
}

size_t icpc_encode(uint8_t msg_type, uint8_t flags, uint32_t seq,
                   uint64_t timestamp_ns, uint16_t err_code,
                   const uint8_t *payload, size_t payload_len,
                   uint8_t *out, size_t out_cap)
{
    if (payload_len > 0xFFFFu || out_cap < ICPC_HEADER_LEN + payload_len)
        return 0;

    out[0] = ICPC_VERSION;
    out[1] = msg_type;
    out[2] = flags;
    out[3] = 0;
    memcpy(out + 4, &seq, 4);
    memcpy(out + 8, &timestamp_ns, 8);
    uint16_t plen = (uint16_t)payload_len;
    memcpy(out + 16, &plen, 2);
    memcpy(out + 18, &err_code, 2);
    memset(out + 20, 0, 4);
    if (payload_len > 0)
        memcpy(out + ICPC_HEADER_LEN, payload, payload_len);

    uint32_t crc = frame_crc(out, out + ICPC_HEADER_LEN, payload_len);
    memcpy(out + 20, &crc, 4);
    return ICPC_HEADER_LEN + payload_len;
}

int icpc_decode(const uint8_t *frame, size_t frame_len, icpc_header_t *hdr,
                const uint8_t **payload_out)
{
    if (frame_len < ICPC_HEADER_LEN || !hdr)
        return -1;

    hdr->version = frame[0];
    if (hdr->version != ICPC_VERSION)
        return -1;
    hdr->msg_type = frame[1];
    hdr->flags = frame[2];
    memcpy(&hdr->seq, frame + 4, 4);
    memcpy(&hdr->timestamp_ns, frame + 8, 8);
    memcpy(&hdr->payload_len, frame + 16, 2);
    memcpy(&hdr->err_code, frame + 18, 2);
    memcpy(&hdr->crc32, frame + 20, 4);

    size_t plen = hdr->payload_len;
    if (frame_len < ICPC_HEADER_LEN + plen)
        return -1;

    uint32_t expected = hdr->crc32;
    uint32_t actual =
        frame_crc(frame, frame + ICPC_HEADER_LEN, plen);
    if (expected != actual)
        return -1;

    if (payload_out)
        *payload_out = frame + ICPC_HEADER_LEN;
    return (int)plen;
}
