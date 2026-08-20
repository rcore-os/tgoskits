/*
 * VTP codec host unit tests.
 *
 * Build & run (container or Linux host, any C11 compiler):
 *
 *   cc -std=c11 -Wall -Wextra -Werror vtp_test.c vtp.c -o vtp_test
 *   ./vtp_test
 *
 * Exit code 0 == all tests passed. A failing assertion prints a FAIL line
 * and the harness exits non-zero, so this is CI-friendly.
 */

#include <stdio.h>
#include <string.h>

#include "vtp.h"

static int g_failures = 0;
static int g_checks = 0;

#define CHECK(cond, what)                                                     \
    do {                                                                      \
        g_checks++;                                                           \
        if (!(cond)) {                                                        \
            g_failures++;                                                     \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, what);    \
        }                                                                     \
    } while (0)

#define CHECK_EQ_INT(a, b, what)                                              \
    do {                                                                      \
        g_checks++;                                                           \
        if ((a) != (b)) {                                                     \
            g_failures++;                                                     \
            fprintf(stderr, "FAIL %s:%d: %s (%d != %d)\n", __FILE__,          \
                    __LINE__, what, (int)(a), (int)(b));                      \
        }                                                                     \
    } while (0)

static void test_crc_known_vector(void)
{
    /* CRC-16/CCITT-FALSE of "123456789" is 0x29B1. */
    const uint8_t input[] = "123456789";
    uint16_t crc = vtp_crc16(input, sizeof(input) - 1);
    CHECK_EQ_INT(crc, 0x29B1u, "crc16 known vector");
}

static void test_control_round_trip(void)
{
    uint8_t wire[256];
    const uint8_t data[] = { 0xde, 0xad, 0xbe, 0xef };
    vtp_header_t hdr;
    const uint8_t *payload;
    uint16_t payload_len;
    uint8_t cmd;
    const uint8_t *pdata;
    uint8_t pdata_len;

    int n = vtp_encode_control(wire, sizeof(wire), VTP_FLAG_REQUEST | VTP_FLAG_ACK_REQUESTED,
                               42u, 1000u, VTP_CMD_SET_STATE, data, sizeof(data));
    CHECK(n == (int)(VTP_HEADER_LEN + 1 + (int)sizeof(data)), "control encode length");

    int rc = vtp_decode(wire, (size_t)n, &hdr, &payload, &payload_len);
    CHECK_EQ_INT(rc, VTP_ERR_OK, "control decode rc");
    CHECK_EQ_INT(hdr.magic, VTP_MAGIC, "magic");
    CHECK_EQ_INT(hdr.version, VTP_VERSION, "version");
    CHECK_EQ_INT(hdr.msg_type, VTP_MSG_CONTROL, "msg_type");
    CHECK_EQ_INT(hdr.flags, VTP_FLAG_REQUEST | VTP_FLAG_ACK_REQUESTED, "flags");
    CHECK_EQ_INT(hdr.seq, 42u, "seq");
    CHECK_EQ_INT(hdr.timestamp_ms, 1000u, "timestamp");

    rc = vtp_parse_control(payload, payload_len, &cmd, &pdata, &pdata_len);
    CHECK_EQ_INT(rc, VTP_ERR_OK, "control parse rc");
    CHECK_EQ_INT(cmd, VTP_CMD_SET_STATE, "control cmd");
    CHECK_EQ_INT(pdata_len, sizeof(data), "control data len");
    CHECK(memcmp(pdata, data, sizeof(data)) == 0, "control data bytes");
}

static void test_bad_checksum(void)
{
    uint8_t wire[256];
    const uint8_t data[] = { 1, 2, 3 };
    vtp_header_t hdr;
    const uint8_t *payload;
    uint16_t payload_len;

    int n = vtp_encode_data(wire, sizeof(wire), 0, 7u, 0u, data, sizeof(data));
    CHECK(n > 0, "data encode");

    wire[VTP_HEADER_LEN] ^= 0xFFu; /* corrupt a payload byte */

    int rc = vtp_decode(wire, (size_t)n, &hdr, &payload, &payload_len);
    CHECK_EQ_INT(rc, -VTP_ERR_BAD_CHECKSUM, "bad checksum detected");
}

static void test_bad_magic(void)
{
    uint8_t wire[256];
    vtp_header_t hdr;
    const uint8_t *payload;
    uint16_t payload_len;

    int n = vtp_encode_data(wire, sizeof(wire), 0, 1u, 0u, NULL, 0);
    CHECK(n > 0, "empty data encode");

    wire[0] ^= 0xFFu;

    int rc = vtp_decode(wire, (size_t)n, &hdr, &payload, &payload_len);
    CHECK_EQ_INT(rc, -VTP_ERR_BAD_MAGIC, "bad magic detected");
}

static void test_bad_version(void)
{
    uint8_t wire[256];
    vtp_header_t hdr;
    const uint8_t *payload;
    uint16_t payload_len;

    int n = vtp_encode_data(wire, sizeof(wire), 0, 1u, 0u, NULL, 0);
    CHECK(n > 0, "empty data encode v2");

    wire[2] = VTP_VERSION + 1;

    int rc = vtp_decode(wire, (size_t)n, &hdr, &payload, &payload_len);
    CHECK_EQ_INT(rc, -VTP_ERR_UNSUPPORTED_VERSION, "bad version detected");
}

static void test_truncated(void)
{
    uint8_t wire[256];
    vtp_header_t hdr;
    const uint8_t *payload;
    uint16_t payload_len;

    int n = vtp_encode_data(wire, sizeof(wire), 0, 1u, 0u, (const uint8_t *)"hello", 5);
    CHECK(n == VTP_HEADER_LEN + 5, "truncated setup encode");

    int rc = vtp_decode(wire, (size_t)(VTP_HEADER_LEN + 2), &hdr, &payload, &payload_len);
    CHECK_EQ_INT(rc, -VTP_ERR_INVALID_PAYLOAD, "truncated datagram rejected");
}

static void test_payload_too_long(void)
{
    uint8_t wire[VTP_HEADER_LEN + VTP_MAX_PAYLOAD + 8];
    vtp_header_t hdr;
    const uint8_t *payload;
    uint16_t payload_len;

    int rc = vtp_encode(wire, sizeof(wire), VTP_MSG_DATA, 0, 1u, 0u, wire,
                        (uint16_t)(VTP_MAX_PAYLOAD + 1));
    CHECK_EQ_INT(rc, -VTP_ERR_INVALID_PAYLOAD, "oversize payload rejected at encode");

    /* Fake a wire datagram declaring an oversized payload length. */
    memset(wire, 0, VTP_HEADER_LEN);
    wire[0] = 0xA5; wire[1] = 0xA5;
    wire[2] = VTP_VERSION;
    wire[14] = (uint8_t)((VTP_MAX_PAYLOAD + 1) >> 8);
    wire[15] = (uint8_t)(VTP_MAX_PAYLOAD + 1);
    rc = vtp_decode(wire, sizeof(wire), &hdr, &payload, &payload_len);
    CHECK_EQ_INT(rc, -VTP_ERR_INVALID_PAYLOAD, "oversize payload rejected at decode");
}

static void test_seq_dedup(void)
{
    vtp_peer_t peer;
    vtp_peer_init(&peer, 100u);

    CHECK_EQ_INT(vtp_tx_seq(&peer), 100u, "first tx seq");
    CHECK_EQ_INT(vtp_tx_seq(&peer), 101u, "second tx seq");

    CHECK_EQ_INT(vtp_rx_accept(&peer, 5u), VTP_ERR_OK, "rx first seq ok");
    CHECK_EQ_INT(vtp_rx_accept(&peer, 5u), -VTP_ERR_SEQ_MISMATCH, "rx duplicate seq rejected");
    CHECK_EQ_INT(vtp_rx_accept(&peer, 6u), VTP_ERR_OK, "rx next seq ok");
}

static void test_typed_messages(void)
{
    uint8_t wire[256];
    vtp_header_t hdr;
    const uint8_t *payload;
    uint16_t payload_len;
    uint8_t state, code, ack;
    uint32_t uptime;
    uint16_t error_code;
    uint8_t source;

    /* STATUS round-trip */
    {
        const uint8_t *extra;
        uint8_t extra_len;
        int n = vtp_encode_status(wire, sizeof(wire), 0, 11u, 500u, VTP_STATE_RUNNING,
                                  0, 12345u, (const uint8_t *)"ok", 2);
        CHECK(n > 0, "status encode");
        CHECK_EQ_INT(vtp_decode(wire, (size_t)n, &hdr, &payload, &payload_len),
                     VTP_ERR_OK, "status decode");
        CHECK_EQ_INT(hdr.msg_type, VTP_MSG_STATUS, "status msg_type");
        CHECK_EQ_INT(vtp_parse_status(payload, payload_len, &state, &code, &uptime,
                                      &extra, &extra_len),
                     VTP_ERR_OK, "status parse");
        CHECK_EQ_INT(state, VTP_STATE_RUNNING, "status state");
        CHECK_EQ_INT(uptime, 12345u, "status uptime");
        CHECK_EQ_INT(extra_len, 2u, "status extra len");
    }

    /* ERROR round-trip */
    {
        const uint8_t *detail;
        uint8_t detail_len;
        int n = vtp_encode_error(wire, sizeof(wire), 0, 12u, 600u, VTP_ERR_BAD_CHECKSUM,
                                 0x42, (const uint8_t *)"crc", 3);
        CHECK(n > 0, "error encode");
        CHECK_EQ_INT(vtp_decode(wire, (size_t)n, &hdr, &payload, &payload_len),
                     VTP_ERR_OK, "error decode");
        CHECK_EQ_INT(vtp_parse_error(payload, payload_len, &error_code, &source,
                                     &detail, &detail_len),
                     VTP_ERR_OK, "error parse");
        CHECK_EQ_INT(error_code, VTP_ERR_BAD_CHECKSUM, "error code");
        CHECK_EQ_INT(source, 0x42, "error source");
    }

    /* ACK round-trip */
    {
        int n = vtp_encode_ack(wire, sizeof(wire), 12u, 700u, 1, VTP_ERR_OK);
        CHECK(n > 0, "ack encode");
        CHECK_EQ_INT(vtp_decode(wire, (size_t)n, &hdr, &payload, &payload_len),
                     VTP_ERR_OK, "ack decode");
        CHECK_EQ_INT(hdr.seq, 12u, "ack echoes request seq");
        CHECK_EQ_INT(vtp_parse_ack(payload, payload_len, &ack, &error_code),
                     VTP_ERR_OK, "ack parse");
        CHECK_EQ_INT(ack, 1, "ack flag");
        CHECK_EQ_INT(error_code, VTP_ERR_OK, "ack error code");
    }
}

int main(void)
{
    test_crc_known_vector();
    test_control_round_trip();
    test_bad_checksum();
    test_bad_magic();
    test_bad_version();
    test_truncated();
    test_payload_too_long();
    test_seq_dedup();
    test_typed_messages();

    if (g_failures != 0) {
        fprintf(stderr, "VTP_TEST_FAIL %d/%d checks failed\n", g_failures, g_checks);
        return 1;
    }
    printf("VTP_TEST_PASS %d checks\n", g_checks);
    return 0;
}
