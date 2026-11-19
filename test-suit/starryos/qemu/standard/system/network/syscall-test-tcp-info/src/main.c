#include "test_framework.h"

#include <netinet/in.h>
#include <netinet/tcp.h>
#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#ifndef TCP_CLOSE
#define TCP_CLOSE 7
#endif

#ifndef TCP_INFO
#define TCP_INFO 11
#endif

struct linux_tcp_info_tail {
    uint8_t tcpi_state;
    uint8_t tcpi_ca_state;
    uint8_t tcpi_retransmits;
    uint8_t tcpi_probes;
    uint8_t tcpi_backoff;
    uint8_t tcpi_options;
    uint8_t tcpi_snd_rcv_wscale;
    uint8_t tcpi_delivery_fastopen;
    uint32_t tcpi_rto;
    uint32_t tcpi_ato;
    uint32_t tcpi_snd_mss;
    uint32_t tcpi_rcv_mss;
    uint32_t tcpi_unacked;
    uint32_t tcpi_sacked;
    uint32_t tcpi_lost;
    uint32_t tcpi_retrans;
    uint32_t tcpi_fackets;
    uint32_t tcpi_last_data_sent;
    uint32_t tcpi_last_ack_sent;
    uint32_t tcpi_last_data_recv;
    uint32_t tcpi_last_ack_recv;
    uint32_t tcpi_pmtu;
    uint32_t tcpi_rcv_ssthresh;
    uint32_t tcpi_rtt;
    uint32_t tcpi_rttvar;
    uint32_t tcpi_snd_ssthresh;
    uint32_t tcpi_snd_cwnd;
    uint32_t tcpi_advmss;
    uint32_t tcpi_reordering;
    uint32_t tcpi_rcv_rtt;
    uint32_t tcpi_rcv_space;
    uint32_t tcpi_total_retrans;
    uint64_t tcpi_pacing_rate;
    uint64_t tcpi_max_pacing_rate;
    uint64_t tcpi_bytes_acked;
    uint64_t tcpi_bytes_received;
    uint32_t tcpi_segs_out;
    uint32_t tcpi_segs_in;
    uint32_t tcpi_notsent_bytes;
    uint32_t tcpi_min_rtt;
    uint32_t tcpi_data_segs_in;
    uint32_t tcpi_data_segs_out;
    uint64_t tcpi_delivery_rate;
    uint64_t tcpi_busy_time;
    uint64_t tcpi_rwnd_limited;
    uint64_t tcpi_sndbuf_limited;
    uint32_t tcpi_delivered;
    uint32_t tcpi_delivered_ce;
    uint64_t tcpi_bytes_sent;
    uint64_t tcpi_bytes_retrans;
    uint32_t tcpi_dsack_dups;
    uint32_t tcpi_reord_seen;
    uint32_t tcpi_rcv_ooopack;
    uint32_t tcpi_snd_wnd;
};

static void test_tcp_info_full_copy(void)
{
    int fd = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    CHECK(fd >= 0, "create TCP socket");
    if (fd < 0) {
        return;
    }

    struct linux_tcp_info_tail info;
    memset(&info, 0xa5, sizeof(info));
    socklen_t len = sizeof(info);

    CHECK_RET(getsockopt(fd, IPPROTO_TCP, TCP_INFO, &info, &len), 0,
              "TCP_INFO succeeds on TCP socket");
    CHECK(len <= sizeof(info), "TCP_INFO returns bounded optlen");
    socklen_t snd_wnd_end =
        offsetof(struct linux_tcp_info_tail, tcpi_snd_wnd) + sizeof(info.tcpi_snd_wnd);
    CHECK(len >= snd_wnd_end,
          "TCP_INFO includes tcpi_snd_wnd");
    CHECK(info.tcpi_state == TCP_CLOSE, "new TCP socket reports TCP_CLOSE");
    CHECK(info.tcpi_snd_wnd == 0, "tcpi_snd_wnd is zero when peer window is unknown");

    close(fd);
}

static void test_tcp_info_short_copy(void)
{
    int fd = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    CHECK(fd >= 0, "create TCP socket for short TCP_INFO copy");
    if (fd < 0) {
        return;
    }

    uint8_t value = 0xa5;
    socklen_t len = sizeof(value);

    CHECK_RET(getsockopt(fd, IPPROTO_TCP, TCP_INFO, &value, &len), 0,
              "TCP_INFO accepts a short optlen");
    CHECK(len == sizeof(value), "TCP_INFO reports the short copied length");
    CHECK(value == TCP_CLOSE, "short TCP_INFO copy returns tcpi_state byte");

    close(fd);
}

static void test_tcp_info_udp_rejected(void)
{
    int fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    CHECK(fd >= 0, "create UDP socket");
    if (fd < 0) {
        return;
    }

    struct linux_tcp_info_tail info;
    socklen_t len = sizeof(info);
    CHECK_ERR(getsockopt(fd, IPPROTO_TCP, TCP_INFO, &info, &len), ENOPROTOOPT,
              "TCP_INFO is rejected on UDP sockets");

    close(fd);
}

int main(void)
{
    TEST_START("getsockopt TCP_INFO semantics");

    test_tcp_info_full_copy();
    test_tcp_info_short_copy();
    test_tcp_info_udp_rejected();

    TEST_DONE();
}
