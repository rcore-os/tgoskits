/* icpc wire format — must match components/icpc (24-byte LE header + CRC32). */
#ifndef TASK2_ICPC_WIRE_H
#define TASK2_ICPC_WIRE_H

#include <stddef.h>
#include <stdint.h>

#define ICPC_HEADER_LEN 24
#define ICPC_VERSION 1
#define ICPC_PORT 9527

#define ICPC_TYPE_CTRL_CMD 0x01
#define ICPC_TYPE_STATE_REPORT 0x02
#define ICPC_TYPE_ERROR_NOTIFY 0x03
#define ICPC_TYPE_ACK 0x04
#define ICPC_TYPE_HEARTBEAT 0x05

#define ICPC_FLAG_NEED_ACK 0x01

#define ICPC_MAX_FRAME 1500

typedef struct {
    uint8_t version;
    uint8_t msg_type;
    uint8_t flags;
    uint32_t seq;
    uint64_t timestamp_ns;
    uint16_t payload_len;
    uint16_t err_code;
    uint32_t crc32;
} icpc_header_t;

/* Returns total frame length, or 0 on error. */
size_t icpc_encode(uint8_t msg_type, uint8_t flags, uint32_t seq,
                   uint64_t timestamp_ns, uint16_t err_code,
                   const uint8_t *payload, size_t payload_len,
                   uint8_t *out, size_t out_cap);

/* Returns payload length, or -1 on error. */
int icpc_decode(const uint8_t *frame, size_t frame_len, icpc_header_t *hdr,
                const uint8_t **payload_out);

uint32_t icpc_crc32(const uint8_t *data, size_t len);

#endif /* TASK2_ICPC_WIRE_H */
