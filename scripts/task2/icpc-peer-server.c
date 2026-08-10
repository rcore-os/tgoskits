#define _GNU_SOURCE
#include "icpc-wire.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <net/if.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>

#include "icpc-pid-plant.h"

#define PEER_IP "10.0.9.3"
#define ACL_SENTINEL_PORT 12345
#define DEDUP_WINDOW 32

#define PID_STEPS_PER_CTRL 100

static uint32_t dedup_slots[DEDUP_WINDOW];
static unsigned dedup_head;
static pid_plant_t g_plant;

static void say(const char *msg);
static int send_frame(int fd, const struct sockaddr_in *peer, socklen_t peer_len,
                      uint8_t msg_type, uint32_t seq, uint64_t ts_ns,
                      uint16_t err_code, const uint8_t *payload, size_t plen);

static int dedup_seen(uint32_t seq)
{
    for (unsigned i = 0; i < DEDUP_WINDOW; i++) {
        if (dedup_slots[i] == seq)
            return 1;
    }
    return 0;
}

static void dedup_remember(uint32_t seq)
{
    dedup_slots[dedup_head] = seq;
    dedup_head = (dedup_head + 1) % DEDUP_WINDOW;
}

static void reply_ctrl(int fd, struct sockaddr_in *peer, socklen_t peer_len,
                       const icpc_header_t *hdr, const uint8_t *payload, size_t plen)
{
    char state_buf[96];
    int is_pid = 0;

    if (payload && plen > 0) {
        if (strstr((const char *)payload, "reset=1") != NULL) {
            pid_plant_init(&g_plant);
            send_frame(fd, peer, peer_len, ICPC_TYPE_STATE_REPORT, hdr->seq,
                       hdr->timestamp_ns, 0, (const uint8_t *)"state=reset", 11);
            say("ICPC_PEER_RESET\n");
            return;
        }
        double kp = g_plant.kp;
        double ki = g_plant.ki;
        double kd = g_plant.kd;
        double sp = g_plant.setpoint;
        if (pid_plant_parse_ctrl((const char *)payload, plen, &kp, &ki, &kd, &sp)) {
            if (strstr((const char *)payload, "setpoint=") != NULL) {
                pid_plant_set_gains(&g_plant, kp, ki, kd, sp);
                for (int step = 0; step < PID_STEPS_PER_CTRL; step++)
                    pid_plant_step(&g_plant);
                is_pid = 1;
                say("ICPC_PEER_PID_TUNED\n");
            } else {
                pid_plant_set_gains(&g_plant, kp, ki, kd, g_plant.setpoint);
            }
        }
    }

    const uint8_t *out_payload;
    size_t out_len;
    if (is_pid) {
        int n = pid_plant_format_state(&g_plant, state_buf, sizeof(state_buf));
        if (n <= 0)
            return;
        out_payload = (const uint8_t *)state_buf;
        out_len = (size_t)n;
    } else {
        out_payload = (const uint8_t *)"state=ok";
        out_len = 8;
    }

    send_frame(fd, peer, peer_len, ICPC_TYPE_STATE_REPORT, hdr->seq, hdr->timestamp_ns,
               0, out_payload, out_len);
    say("ICPC_PEER_STATE\n");
}

static void reply_error_ack(int fd, struct sockaddr_in *peer, socklen_t peer_len,
                            const icpc_header_t *hdr)
{
    send_frame(fd, peer, peer_len, ICPC_TYPE_ACK, hdr->seq, hdr->timestamp_ns, 0, NULL,
               0);
    say("ICPC_PEER_ACK\n");
}

static void say(const char *msg)
{
    int fd = open("/dev/console", O_WRONLY | O_NOCTTY);
    if (fd < 0)
        fd = open("/dev/ttyAMA0", O_WRONLY | O_NOCTTY);
    if (fd < 0)
        fd = 1;
    write(fd, msg, strlen(msg));
    if (fd > 2)
        close(fd);
    write(1, msg, strlen(msg));
}

static int configure_eth0(void)
{
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0)
        return -1;

    struct ifreq ifr;
    memset(&ifr, 0, sizeof(ifr));
    strncpy(ifr.ifr_name, "eth0", IFNAMSIZ - 1);

    if (ioctl(fd, SIOCGIFFLAGS, &ifr) < 0) {
        close(fd);
        return -1;
    }
    ifr.ifr_flags |= IFF_UP | IFF_RUNNING;
    if (ioctl(fd, SIOCSIFFLAGS, &ifr) < 0) {
        close(fd);
        return -1;
    }

    struct sockaddr_in *addr = (struct sockaddr_in *)&ifr.ifr_addr;
    addr->sin_family = AF_INET;
    if (inet_pton(AF_INET, PEER_IP, &addr->sin_addr) != 1) {
        close(fd);
        return -1;
    }
    if (ioctl(fd, SIOCSIFADDR, &ifr) < 0) {
        close(fd);
        return -1;
    }

    addr->sin_addr.s_addr = htonl(0xffffff00u);
    if (ioctl(fd, SIOCSIFNETMASK, &ifr) < 0) {
        close(fd);
        return -1;
    }

    close(fd);
    return 0;
}

static int send_frame(int fd, const struct sockaddr_in *peer, socklen_t peer_len,
                      uint8_t msg_type, uint32_t seq, uint64_t ts_ns,
                      uint16_t err_code, const uint8_t *payload, size_t plen)
{
    uint8_t out[ICPC_MAX_FRAME];
    size_t n = icpc_encode(msg_type, 0, seq, ts_ns, err_code, payload, plen, out,
                           sizeof(out));
    if (n == 0)
        return -1;
    return (int)sendto(fd, out, n, 0, (const struct sockaddr *)peer, peer_len);
}

static void handle_datagram(int fd, const uint8_t *buf, ssize_t n,
                            struct sockaddr_in *peer, socklen_t peer_len)
{
    icpc_header_t hdr;
    const uint8_t *payload = NULL;
    int plen = icpc_decode(buf, (size_t)n, &hdr, &payload);

    if (plen < 0) {
        /* Plain-text compat for vsw-dual-guest udp-probe. */
        if (n > 0)
            sendto(fd, buf, (size_t)n, 0, (struct sockaddr *)peer, peer_len);
        return;
    }

    switch (hdr.msg_type) {
    case ICPC_TYPE_CTRL_CMD:
        if (dedup_seen(hdr.seq)) {
            reply_ctrl(fd, peer, peer_len, &hdr, payload,
                       plen > 0 ? (size_t)plen : 0);
            break;
        }
        dedup_remember(hdr.seq);
        say("ICPC_PEER_CTRL\n");
        reply_ctrl(fd, peer, peer_len, &hdr, payload, plen > 0 ? (size_t)plen : 0);
        break;
    case ICPC_TYPE_ERROR_NOTIFY:
        if (dedup_seen(hdr.seq)) {
            reply_error_ack(fd, peer, peer_len, &hdr);
            break;
        }
        dedup_remember(hdr.seq);
        say("ICPC_PEER_ERROR\n");
        reply_error_ack(fd, peer, peer_len, &hdr);
        break;
    case ICPC_TYPE_STATE_REPORT:
        say("ICPC_PEER_RX_STATE\n");
        break;
    case ICPC_TYPE_HEARTBEAT:
        send_frame(fd, peer, peer_len, ICPC_TYPE_HEARTBEAT, hdr.seq,
                   hdr.timestamp_ns, 0, NULL, 0);
        break;
    default:
        break;
    }
}

static void icpc_serve_forever(void)
{
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0)
        return;

    struct sockaddr_in bind_addr;
    memset(&bind_addr, 0, sizeof(bind_addr));
    bind_addr.sin_family = AF_INET;
    bind_addr.sin_addr.s_addr = htonl(INADDR_ANY);
    bind_addr.sin_port = htons(ICPC_PORT);
    if (bind(fd, (struct sockaddr *)&bind_addr, sizeof(bind_addr)) < 0) {
        close(fd);
        return;
    }

    for (;;) {
        uint8_t buf[ICPC_MAX_FRAME];
        struct sockaddr_in peer;
        socklen_t peer_len = sizeof(peer);
        ssize_t n = recvfrom(fd, buf, sizeof(buf), 0, (struct sockaddr *)&peer, &peer_len);
        if (n > 0)
            handle_datagram(fd, buf, n, &peer, peer_len);
    }
}

static void acl_sentinel_forever(void)
{
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0)
        return;

    struct sockaddr_in bind_addr;
    memset(&bind_addr, 0, sizeof(bind_addr));
    bind_addr.sin_family = AF_INET;
    bind_addr.sin_addr.s_addr = htonl(INADDR_ANY);
    bind_addr.sin_port = htons(ACL_SENTINEL_PORT);
    if (bind(fd, (struct sockaddr *)&bind_addr, sizeof(bind_addr)) < 0) {
        close(fd);
        return;
    }

    for (;;) {
        uint8_t buf[64];
        struct sockaddr_in peer;
        socklen_t peer_len = sizeof(peer);
        ssize_t n = recvfrom(fd, buf, sizeof(buf), 0, (struct sockaddr *)&peer, &peer_len);
        if (n > 0)
            say("ICPC_ACL_LEAK\n");
    }
}

int main(void)
{
    write(1, "ICPC_PEER_START\n", 16);

    mount("devtmpfs", "/dev", "devtmpfs", 0, NULL);
    mount("proc", "/proc", "proc", 0, NULL);
    mount("sysfs", "/sys", "sysfs", 0, NULL);
    say("ICPC_PEER_MOUNTED\n");

    for (int i = 0; i < 150; i++) {
        if (access("/sys/class/net/eth0", F_OK) == 0)
            break;
        usleep(100000);
    }

    if (access("/sys/class/net/eth0", F_OK) != 0)
        say("ICPC_PEER_NO_ETH0\n");

    if (configure_eth0() != 0)
        say("ICPC_PEER_ETH_FAIL\n");
    else
        say("ICPC_PEER_READY\n");

    pid_plant_init(&g_plant);

    if (fork() == 0) {
        icpc_serve_forever();
        _exit(0);
    }

    if (fork() == 0) {
        acl_sentinel_forever();
        _exit(0);
    }

    if (fork() == 0) {
        unsigned long last_rx = 0;
        for (;;) {
            unsigned long rx = 0, drop = 0, err = 0;
            FILE *f;
            if ((f = fopen("/sys/class/net/eth0/statistics/rx_packets", "r"))) {
                if (fscanf(f, "%lu", &rx) != 1)
                    rx = 0;
                fclose(f);
            }
            if ((f = fopen("/sys/class/net/eth0/statistics/rx_dropped", "r"))) {
                if (fscanf(f, "%lu", &drop) != 1)
                    drop = 0;
                fclose(f);
            }
            if ((f = fopen("/sys/class/net/eth0/statistics/rx_errors", "r"))) {
                if (fscanf(f, "%lu", &err) != 1)
                    err = 0;
                fclose(f);
            }
            if (rx != last_rx || drop || err) {
                char msg[128];
                int m = snprintf(msg, sizeof(msg),
                                 "ICPC_PEER_RX=%lu drop=%lu err=%lu\n", rx, drop,
                                 err);
                if (m > 0)
                    say(msg);
                last_rx = rx;
            }
            sleep(1);
        }
    }

    for (;;)
        pause();
    return 0;
}
