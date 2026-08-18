/*
 * VTP - Virtual Transport Protocol
 *
 * Application-layer protocol running over UDP between a StarryOS/Linux guest
 * and an RTOS (FreeRTOS) guest, bridged by the Axvisor internal L2 switch.
 *
 * This codec is the single source of truth shared by both endpoints:
 *   - StarryOS side (POSIX sockets)
 *   - FreeRTOS side (lwIP sockets)
 *
 * It is self-contained and allocation-free so it compiles equally well in a
 * hosted C environment and a bare-metal RTOS environment.
 *
 * Wire format (all multi-byte fields network byte order):
 *
 *   offset size field
 *   0      2   magic = 0xA5A5
 *   2      1   version = 0x01
 *   3      1   msg_type   CONTROL=0x01 STATUS=0x02 DATA=0x03 ERROR=0x04 ACK=0x05
 *   4      1   flags      bit0 REQUEST, bit1 LAST_FRAGMENT, bit2 ACK_REQUESTED
 *   5      1   reserved   (must be 0)
 *   6      4   seq        monotonically increasing sender sequence
 *   10     4   timestamp_ms  sender monotonic clock (ms)
 *   14     2   payload_len   <= VTP_MAX_PAYLOAD
 *   16     2   checksum      CRC16-CCITT over bytes [0,16) + payload
 *   18     n   payload
 *
 * Canonical protocol definition: docs/design/axvisor-vtp.md
 */

#ifndef VTP_H
#define VTP_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ------------------------------------------------------------------ */
/* Constants                                                          */
/* ------------------------------------------------------------------ */

#define VTP_MAGIC            0xA5A5u
#define VTP_VERSION          0x01u
#define VTP_HEADER_LEN       18u
#define VTP_MAX_PAYLOAD      1400u /* keeps UDP datagram below common MTU */

/* Message types */
#define VTP_MSG_CONTROL      0x01u
#define VTP_MSG_STATUS       0x02u
#define VTP_MSG_DATA         0x03u
#define VTP_MSG_ERROR        0x04u
#define VTP_MSG_ACK          0x05u

/* Flags */
#define VTP_FLAG_REQUEST       (1u << 0)
#define VTP_FLAG_LAST_FRAGMENT (1u << 1)
#define VTP_FLAG_ACK_REQUESTED (1u << 2)

/* Control command codes (CONTROL payload[0]) */
#define VTP_CMD_PING          0x01u
#define VTP_CMD_SET_STATE     0x02u
#define VTP_CMD_REQ_STATUS    0x03u
#define VTP_CMD_RESET         0x04u

/* Status states (STATUS payload[0]) */
#define VTP_STATE_INIT        0u
#define VTP_STATE_READY       1u
#define VTP_STATE_RUNNING     2u
#define VTP_STATE_DEGRADED    3u
#define VTP_STATE_MAINTENANCE 4u
#define VTP_STATE_ERROR       5u

/* Error codes (negative return values / ERROR payload error_code) */
#define VTP_ERR_OK                    0x0000
#define VTP_ERR_UNSUPPORTED_VERSION   0x0001
#define VTP_ERR_BAD_CHECKSUM          0x0002
#define VTP_ERR_UNKNOWN_CMD           0x0003
#define VTP_ERR_INVALID_PAYLOAD       0x0004
#define VTP_ERR_SEQ_MISMATCH          0x0005
#define VTP_ERR_TIMEOUT               0x0006
#define VTP_ERR_NOT_READY             0x0007
#define VTP_ERR_RESOURCE_BUSY         0x0008
#define VTP_ERR_BAD_MAGIC             0x0009

/* ------------------------------------------------------------------ */
/* Wire header                                                        */
/* ------------------------------------------------------------------ */

typedef struct __attribute__((packed)) {
    uint16_t magic;
    uint8_t version;
    uint8_t msg_type;
    uint8_t flags;
    uint8_t reserved;
    uint32_t seq;
    uint32_t timestamp_ms;
    uint16_t payload_len;
    uint16_t checksum;
} vtp_header_t;

_Static_assert(sizeof(vtp_header_t) == VTP_HEADER_LEN,
               "vtp_header_t must be 18 bytes");

/* ------------------------------------------------------------------ */
/* Peer state (seq ordering / dedup)                                  */
/* ------------------------------------------------------------------ */

typedef struct {
    uint32_t next_tx_seq;
    uint32_t last_rx_seq;
    int rx_initialized;
} vtp_peer_t;

void vtp_peer_init(vtp_peer_t *peer, uint32_t first_tx_seq);

/* Returns the next TX sequence and advances the counter. */
uint32_t vtp_tx_seq(vtp_peer_t *peer);

/* Returns VTP_ERR_OK when `seq` is a new, in-order-or-newer sequence, or a
 * negative VTP error (VTP_ERR_SEQ_MISMATCH) when it is a duplicate. */
int vtp_rx_accept(vtp_peer_t *peer, uint32_t seq);

/* ------------------------------------------------------------------ */
/* CRC16-CCITT (poly 0x1021, init 0xFFFF, no final xor)               */
/* ------------------------------------------------------------------ */

uint16_t vtp_crc16(const uint8_t *data, size_t len);

/* Continue a CRC over a next buffer segment (see vtp_crc16 for init). */
uint16_t vtp_crc16_update(uint16_t crc, const uint8_t *data, size_t len);

/* ------------------------------------------------------------------ */
/* Core encode / decode                                               */
/* ------------------------------------------------------------------ */

/* Encodes a full VTP datagram into `out` (capacity `cap`). On success returns
 * the total datagram length (VTP_HEADER_LEN + payload_len), otherwise a
 * negative VTP error code. */
int vtp_encode(uint8_t *out, size_t cap, uint8_t msg_type, uint8_t flags,
               uint32_t seq, uint32_t timestamp_ms, const uint8_t *payload,
               uint16_t payload_len);

/* Validates a full inbound datagram `buf`/`len`. On success fills `hdr`
 * (host order), points `out_payload`/`out_payload_len` into the datagram and
 * returns VTP_ERR_OK. Returns a negative VTP error code otherwise. */
int vtp_decode(const uint8_t *buf, size_t len, vtp_header_t *hdr,
               const uint8_t **out_payload, uint16_t *out_payload_len);

/* ------------------------------------------------------------------ */
/* Typed message builders (wrap vtp_encode with typed payloads)       */
/* ------------------------------------------------------------------ */

/* CONTROL: payload = { cmd, data[0..data_len] } */
int vtp_encode_control(uint8_t *out, size_t cap, uint8_t flags, uint32_t seq,
                       uint32_t timestamp_ms, uint8_t cmd, const uint8_t *data,
                       uint8_t data_len);

/* STATUS: payload = { state, code, uptime_ms(be32), extra[0..extra_len] } */
int vtp_encode_status(uint8_t *out, size_t cap, uint8_t flags, uint32_t seq,
                      uint32_t timestamp_ms, uint8_t state, uint8_t code,
                      uint32_t uptime_ms, const uint8_t *extra,
                      uint8_t extra_len);

/* ERROR: payload = { error_code(be16), source, detail[0..detail_len] } */
int vtp_encode_error(uint8_t *out, size_t cap, uint8_t flags, uint32_t seq,
                     uint32_t timestamp_ms, uint16_t error_code, uint8_t source,
                     const uint8_t *detail, uint8_t detail_len);

/* ACK: payload = { ack, error_code(be16) }; echoes the request seq. */
int vtp_encode_ack(uint8_t *out, size_t cap, uint32_t seq,
                   uint32_t timestamp_ms, uint8_t ack, uint16_t error_code);

/* DATA: payload = raw bytes. */
int vtp_encode_data(uint8_t *out, size_t cap, uint8_t flags, uint32_t seq,
                    uint32_t timestamp_ms, const uint8_t *data, uint16_t len);

/* ------------------------------------------------------------------ */
/* Typed message parsers                                              */
/* ------------------------------------------------------------------ */

int vtp_parse_control(const uint8_t *payload, uint16_t len, uint8_t *cmd,
                      const uint8_t **data, uint8_t *data_len);

int vtp_parse_status(const uint8_t *payload, uint16_t len, uint8_t *state,
                     uint8_t *code, uint32_t *uptime_ms, const uint8_t **extra,
                     uint8_t *extra_len);

int vtp_parse_error(const uint8_t *payload, uint16_t len, uint16_t *error_code,
                    uint8_t *source, const uint8_t **detail, uint8_t *detail_len);

int vtp_parse_ack(const uint8_t *payload, uint16_t len, uint8_t *ack,
                  uint16_t *error_code);

#ifdef __cplusplus
}
#endif

#endif /* VTP_H */
