/*
 * FreeRTOS VTP agent.
 *
 * Runs the FreeRTOS side of the VTP demo over lwIP UDP sockets. It answers
 * CONTROL REQ_STATUS with a STATUS (+ ACK), echoes DATA back, and periodically
 * pushes its own STATUS and DATA to the StarryOS peer. Prints
 * FREERTOS_VTP_PASS when the handshake completes, FREERTOS_VTP_FAIL on timeout.
 *
 * Integrate by spawning a task that calls vtp_agent_task(). The netif must be
 * added first via vt_netif_add(); a periodic task should call vt_netif_poll()
 * to drain the virtio RX queue into lwIP.
 */

#include <stdio.h>
#include <string.h>

#include "vtp.h"
#include "lwip_netif.h"

#include "lwip/opt.h"
#include "lwip/inet.h"
#include "lwip/ip_addr.h"
#include "lwip/sockets.h"
#include "lwip/sys.h"

#include "FreeRTOS.h"
#include "task.h"

#define VTP_PORT 6000
#define PEER_IP "10.0.2.15"
#define REQUIRED_ROUNDS 5
#define RUN_MS 90000u
#define RECV_TIMEOUT_MS 500u
#define SEND_INTERVAL_MS 1000u

static uint32_t vtp_agent_now_ms(void)
{
    return (uint32_t)sys_now();
}

/* Send a STATUS report toward the StarryOS peer. */
static void vtp_agent_send_status(int fd, const struct sockaddr_in *peer,
                                  vtp_peer_t *vtp)
{
    uint8_t wire[512];
    uint32_t seq = vtp_tx_seq(vtp);
    int n = vtp_encode_status(wire, sizeof(wire), VTP_FLAG_REQUEST, seq,
                              vtp_agent_now_ms(), VTP_STATE_RUNNING, 0,
                              (uint32_t)sys_now(), (const uint8_t *)"rtos", 4);
    if (n > 0) {
        lwip_sendto(fd, wire, (size_t)n, 0, (const struct sockaddr *)peer,
                    sizeof(*peer));
    }
}

/* Send an ACK for a CONTROL request (echoes its seq). */
static void vtp_agent_send_ack(int fd, const struct sockaddr_in *peer,
                               uint32_t req_seq, uint16_t error_code)
{
    uint8_t wire[512];
    int n = vtp_encode_ack(wire, sizeof(wire), req_seq, vtp_agent_now_ms(), 1,
                           error_code);
    if (n > 0) {
        lwip_sendto(fd, wire, (size_t)n, 0, (const struct sockaddr *)peer,
                    sizeof(*peer));
    }
}

/* Send an ERROR notification toward the peer. */
static void vtp_agent_send_error(int fd, const struct sockaddr_in *peer,
                                 vtp_peer_t *vtp, uint16_t error_code)
{
    uint8_t wire[512];
    int n = vtp_encode_error(wire, sizeof(wire), VTP_FLAG_REQUEST, vtp_tx_seq(vtp),
                             vtp_agent_now_ms(), error_code, 0x52 /* RTOS */,
                             (const uint8_t *)"badframe", 8);
    if (n > 0) {
        lwip_sendto(fd, wire, (size_t)n, 0, (const struct sockaddr *)peer,
                    sizeof(*peer));
    }
}

void vtp_agent_task(void *arg)
{
    int fd;
    struct sockaddr_in bind_addr;
    struct sockaddr_in peer_addr;
    struct timeval tv;
    vtp_peer_t vtp;
    uint8_t wire[VTP_HEADER_LEN + VTP_MAX_PAYLOAD];
    uint32_t deadline;
    uint32_t last_send_ms;
    int rounds = 0;
    int saw_data = 0;
    int sent = 0;

    (void)arg;

    printf("FREERTOS_VTP_READY ip=10.0.2.16\n");

    fd = lwip_socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) {
        printf("FREERTOS_VTP_FAIL socket\n");
        vTaskDelete(NULL);
        return;
    }

    memset(&bind_addr, 0, sizeof(bind_addr));
    bind_addr.sin_family = AF_INET;
    bind_addr.sin_port = htons(VTP_PORT);
    bind_addr.sin_addr.s_addr = htonl(INADDR_ANY);
    if (lwip_bind(fd, (const struct sockaddr *)&bind_addr, sizeof(bind_addr)) < 0) {
        printf("FREERTOS_VTP_FAIL bind\n");
        lwip_close(fd);
        vTaskDelete(NULL);
        return;
    }

    memset(&peer_addr, 0, sizeof(peer_addr));
    peer_addr.sin_family = AF_INET;
    peer_addr.sin_port = htons(VTP_PORT);
    peer_addr.sin_addr.s_addr = inet_addr(PEER_IP);

    tv.tv_sec = 0;
    tv.tv_usec = RECV_TIMEOUT_MS * 1000;
    lwip_setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));

    vtp_peer_init(&vtp, 1);
    deadline = vtp_agent_now_ms() + RUN_MS;
    last_send_ms = vtp_agent_now_ms();

    for (;;) {
        uint32_t now = vtp_agent_now_ms();

        if (now >= deadline) {
            printf("FREERTOS_VTP_FAIL rounds=%d data=%d sent=%d\n", rounds,
                   saw_data, sent);
            lwip_close(fd);
            vTaskDelete(NULL);
            return;
        }

        /* Periodically push STATUS + DATA toward StarryOS. */
        if (now - last_send_ms >= SEND_INTERVAL_MS) {
            uint8_t data[64];
            int n;

            vtp_agent_send_status(fd, &peer_addr, &vtp);
            snprintf(data, sizeof(data), "rtos-data-%u", vtp_tx_seq(&vtp));
            n = vtp_encode_data(wire, sizeof(wire), VTP_FLAG_REQUEST, vtp_tx_seq(&vtp),
                                now, (const uint8_t *)data, (uint16_t)strlen(data));
            if (n > 0) {
                lwip_sendto(fd, wire, (size_t)n, 0, (const struct sockaddr *)&peer_addr,
                            sizeof(peer_addr));
            }
            sent++;
            last_send_ms = now;
        }

        {
            struct sockaddr_in from;
            socklen_t from_len = sizeof(from);
            int n = lwip_recvfrom(fd, wire, sizeof(wire), 0,
                                  (struct sockaddr *)&from, &from_len);
            if (n < 0) {
                vTaskDelay(pdMS_TO_TICKS(10));
                continue;
            }
            if (n < (int)VTP_HEADER_LEN) {
                continue;
            }

            vtp_header_t hdr;
            const uint8_t *payload;
            uint16_t payload_len;
            int rc = vtp_decode(wire, (size_t)n, &hdr, &payload, &payload_len);
            if (rc < 0) {
                printf("FREERTOS_VTP_ERROR decode rc=%d\n", rc);
                vtp_agent_send_error(fd, &from, &vtp, (uint16_t)(-rc));
                continue;
            }

            switch (hdr.msg_type) {
            case VTP_MSG_CONTROL: {
                uint8_t cmd;
                const uint8_t *data;
                uint8_t data_len;
                if (vtp_parse_control(payload, payload_len, &cmd, &data, &data_len) !=
                    VTP_ERR_OK) {
                    break;
                }
                if (cmd == VTP_CMD_REQ_STATUS) {
                    vtp_agent_send_status(fd, &from, &vtp);
                    vtp_agent_send_ack(fd, &from, hdr.seq, VTP_ERR_OK);
                    rounds++;
                    printf("FREERTOS_VTP_STATUS rounds=%d\n", rounds);
                } else if (cmd == VTP_CMD_PING || cmd == VTP_CMD_SET_STATE) {
                    vtp_agent_send_ack(fd, &from, hdr.seq, VTP_ERR_OK);
                } else {
                    vtp_agent_send_ack(fd, &from, hdr.seq, VTP_ERR_UNKNOWN_CMD);
                }
                break;
            }
            case VTP_MSG_DATA:
                saw_data++;
                printf("FREERTOS_VTP_DATA seq=%u len=%u\n", hdr.seq, payload_len);
                break;
            case VTP_MSG_ERROR: {
                uint16_t ec;
                uint8_t source;
                const uint8_t *detail;
                uint8_t detail_len;
                if (vtp_parse_error(payload, payload_len, &ec, &source, &detail,
                                    &detail_len) == VTP_ERR_OK) {
                    printf("FREERTOS_VTP_ERROR_NOTIFY code=0x%x source=%u\n", ec,
                           source);
                }
                break;
            }
            default:
                break;
            }

            if (rounds >= REQUIRED_ROUNDS && saw_data >= 1) {
                printf("FREERTOS_VTP_PASS rounds=%d data=%d\n", rounds, saw_data);
                lwip_close(fd);
                vTaskDelete(NULL);
                return;
            }
        }
    }
}
