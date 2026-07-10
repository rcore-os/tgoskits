/*
 * ip-mtu-discover-udp-flush.c - Regression for the two UDP kernel gaps dnsmasq
 * TFTP exposed, fixed against Linux net/ipv4/ip_sockglue.c and the UDP close
 * path.
 *
 *   1. IP_MTU_DISCOVER setsockopt/getsockopt ABI: a fresh datagram socket
 *      reports IP_PMTUDISC_WANT (Linux UDP default); every mode in
 *      IP_PMTUDISC_DONT..OMIT round-trips through set/getsockopt; a mode above
 *      IP_PMTUDISC_OMIT is rejected with EINVAL (ip_sock_set_mtu_discover /
 *      do_ip_setsockopt validate the range). dnsmasq sets IP_PMTUDISC_DONT on
 *      its transfer socket and aborts the transfer if the call fails.
 *   2. UDP egress flush before close: a datagram sent and immediately followed
 *      by close(2) must still reach the peer. StarryOS UdpSocket::drop flushes
 *      queued egress before smoltcp drops the send buffer, matching Linux which
 *      keeps the datagram in the peer's receive buffer once queued. A regressed
 *      build drops the datagram and the receiver times out.
 *
 * Loopback only, single process; deterministic.
 */

#include "test_framework.h"

#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

#ifndef IP_MTU_DISCOVER
#define IP_MTU_DISCOVER 10
#endif
#ifndef IP_PMTUDISC_DONT
#define IP_PMTUDISC_DONT 0
#define IP_PMTUDISC_WANT 1
#define IP_PMTUDISC_DO 2
#define IP_PMTUDISC_PROBE 3
#define IP_PMTUDISC_INTERFACE 4
#define IP_PMTUDISC_OMIT 5
#endif

/* 1. IP_MTU_DISCOVER default + set/get round-trip + out-of-range EINVAL. */
static void test_ip_mtu_discover_abi(void)
{
    int s = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(s >= 0, "open UDP socket for IP_MTU_DISCOVER");
    if (s < 0)
        return;

    int mode = -1;
    socklen_t len = sizeof(mode);
    CHECK_RET(getsockopt(s, IPPROTO_IP, IP_MTU_DISCOVER, &mode, &len), 0,
              "getsockopt IP_MTU_DISCOVER on a fresh socket");
    CHECK(mode == IP_PMTUDISC_WANT, "fresh UDP socket defaults to IP_PMTUDISC_WANT");

    const int modes[] = {IP_PMTUDISC_DONT, IP_PMTUDISC_WANT, IP_PMTUDISC_DO,
                         IP_PMTUDISC_PROBE, IP_PMTUDISC_INTERFACE, IP_PMTUDISC_OMIT};
    for (unsigned i = 0; i < sizeof(modes) / sizeof(modes[0]); i++)
    {
        int want = modes[i];
        CHECK_RET(setsockopt(s, IPPROTO_IP, IP_MTU_DISCOVER, &want, sizeof(want)), 0,
                  "setsockopt IP_MTU_DISCOVER accepts a valid mode");
        int got = -1;
        len = sizeof(got);
        CHECK_RET(getsockopt(s, IPPROTO_IP, IP_MTU_DISCOVER, &got, &len), 0,
                  "getsockopt IP_MTU_DISCOVER after set");
        CHECK(got == want, "IP_MTU_DISCOVER mode round-trips through set/get");
    }

    /* A mode above IP_PMTUDISC_OMIT is not defined; Linux rejects it. */
    int bad = IP_PMTUDISC_OMIT + 1;
    CHECK_ERR(setsockopt(s, IPPROTO_IP, IP_MTU_DISCOVER, &bad, sizeof(bad)), EINVAL,
              "setsockopt IP_MTU_DISCOVER rejects an out-of-range mode with EINVAL");

    close(s);
}

/* 2. A datagram sent then immediately closed must still reach the peer. */
static void test_udp_egress_flush_before_close(void)
{
    int rx = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(rx >= 0, "open UDP receiver");
    if (rx < 0)
        return;

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr.sin_port = 0; /* ephemeral */
    CHECK_RET(bind(rx, (struct sockaddr *)&addr, sizeof(addr)), 0, "bind receiver to loopback");

    socklen_t alen = sizeof(addr);
    CHECK_RET(getsockname(rx, (struct sockaddr *)&addr, &alen), 0, "getsockname receiver port");

    /* Bound the receive so a regressed (dropped) datagram fails fast instead of
     * hanging the whole suite. */
    struct timeval tv = {.tv_sec = 2, .tv_usec = 0};
    setsockopt(rx, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));

    int tx = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(tx >= 0, "open UDP sender");
    if (tx < 0)
    {
        close(rx);
        return;
    }

    const char *payload = "tftp-last-block";
    ssize_t sent = sendto(tx, payload, strlen(payload), 0,
                          (struct sockaddr *)&addr, sizeof(addr));
    CHECK(sent == (ssize_t)strlen(payload), "sendto queues the datagram");
    /* Close the sender right away: the egress flush in UdpSocket::drop must run
     * before smoltcp drops the send buffer, or the datagram is lost. */
    close(tx);

    char buf[64] = {0};
    ssize_t got = recv(rx, buf, sizeof(buf), 0);
    CHECK(got == (ssize_t)strlen(payload),
          "datagram sent-then-closed is flushed and received (not dropped)");
    CHECK(got > 0 && memcmp(buf, payload, (size_t)got) == 0, "flushed payload round-trips intact");

    close(rx);
}

int main(void)
{
    TEST_START("IP_MTU_DISCOVER ABI + UDP egress flush before close");

    test_ip_mtu_discover_abi();
    test_udp_egress_flush_before_close();

    TEST_DONE();
}
