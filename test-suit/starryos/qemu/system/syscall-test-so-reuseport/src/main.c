// SO_REUSEPORT regression test.
//
// StarryOS used to reject SO_REUSEPORT with ENOPROTOOPT (unrecognized option),
// which breaks Envoy/higress since Envoy enables SO_REUSEPORT on every listener
// by default. This asserts the option round-trips, that a reuseport group may
// share a bound port while a plain binder is refused with EADDRINUSE, and that
// every group member may also listen() on the shared port (one listener per
// worker), while a plain socket still cannot join the reuseport listener port.
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "test_framework.h"
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>

static struct sockaddr_in loopback(unsigned short port) {
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons(port);
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    return addr;
}

static int get_reuseport(int fd) {
    int val = -1;
    socklen_t len = sizeof(val);
    if (getsockopt(fd, SOL_SOCKET, SO_REUSEPORT, &val, &len) != 0) {
        return -1;
    }
    return val;
}

static int set_reuseport(int fd, int on) {
    return setsockopt(fd, SOL_SOCKET, SO_REUSEPORT, &on, sizeof(on));
}

// SO_REUSEPORT is accepted and its stored value round-trips through getsockopt.
static void test_option_roundtrip(void) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(fd >= 0, "create TCP socket");

    CHECK_RET(get_reuseport(fd), 0, "SO_REUSEPORT defaults to 0");
    // The core fix: this used to fail with ENOPROTOOPT.
    CHECK_RET(set_reuseport(fd, 1), 0, "setsockopt SO_REUSEPORT=1 succeeds");
    CHECK_RET(get_reuseport(fd), 1, "getsockopt reads SO_REUSEPORT back as 1");
    CHECK_RET(set_reuseport(fd, 0), 0, "setsockopt SO_REUSEPORT=0 succeeds");
    CHECK_RET(get_reuseport(fd), 0, "getsockopt reads SO_REUSEPORT back as 0");

    close(fd);
}

// Two TCP sockets that both request SO_REUSEPORT may bind the same address:port;
// a third socket without SO_REUSEPORT is refused with EADDRINUSE.
static void test_tcp_reuseport_group(void) {
    const unsigned short port = 18099;
    struct sockaddr_in addr = loopback(port);

    int a = socket(AF_INET, SOCK_STREAM, 0);
    int b = socket(AF_INET, SOCK_STREAM, 0);
    int c = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(a >= 0 && b >= 0 && c >= 0, "create three TCP sockets");

    CHECK_RET(set_reuseport(a, 1), 0, "socket a: SO_REUSEPORT=1");
    CHECK_RET(bind(a, (struct sockaddr *)&addr, sizeof(addr)), 0, "socket a binds the port");

    CHECK_RET(set_reuseport(b, 1), 0, "socket b: SO_REUSEPORT=1");
    CHECK_RET(bind(b, (struct sockaddr *)&addr, sizeof(addr)), 0,
              "socket b joins the reuseport group on the same port");

    // No SO_REUSEPORT: must not be allowed to steal the reuseport-owned port.
    CHECK_ERR(bind(c, (struct sockaddr *)&addr, sizeof(addr)), EADDRINUSE,
              "plain socket c cannot bind the reuseport-owned port");

    close(a);
    close(b);
    close(c);
}

// Plain double-bind on the same port is still rejected once nobody in the group
// requested SO_REUSEPORT (guards against the option becoming a blanket bypass).
static void test_plain_bind_still_conflicts(void) {
    const unsigned short port = 18100;
    struct sockaddr_in addr = loopback(port);

    int a = socket(AF_INET, SOCK_STREAM, 0);
    int b = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(a >= 0 && b >= 0, "create two TCP sockets");

    CHECK_RET(bind(a, (struct sockaddr *)&addr, sizeof(addr)), 0, "socket a binds the port");
    CHECK_ERR(bind(b, (struct sockaddr *)&addr, sizeof(addr)), EADDRINUSE,
              "socket b without SO_REUSEPORT is refused");

    close(a);
    close(b);
}

// A reuseport group may hold several *listening* sockets on the same port. The
// bind layer already lets them share the port; the listener table must accept
// every member's listen() too, otherwise the second listen() wrongly returns
// EADDRINUSE. Envoy/higress rely on this to run one listener per worker.
static void test_tcp_reuseport_listen_group(void) {
    const unsigned short port = 18103;
    struct sockaddr_in addr = loopback(port);

    int a = socket(AF_INET, SOCK_STREAM, 0);
    int b = socket(AF_INET, SOCK_STREAM, 0);
    int c = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(a >= 0 && b >= 0 && c >= 0, "create three TCP sockets");

    CHECK_RET(set_reuseport(a, 1), 0, "listener a: SO_REUSEPORT=1");
    CHECK_RET(bind(a, (struct sockaddr *)&addr, sizeof(addr)), 0, "listener a binds the port");
    CHECK_RET(listen(a, 16), 0, "listener a listens on the port");

    CHECK_RET(set_reuseport(b, 1), 0, "listener b: SO_REUSEPORT=1");
    CHECK_RET(bind(b, (struct sockaddr *)&addr, sizeof(addr)), 0,
              "listener b joins the reuseport group on the same port");
    // The core listener-layer fix: the second listen() must not fail EADDRINUSE.
    CHECK_RET(listen(b, 16), 0, "listener b also listens on the shared reuseport port");

    // A socket without SO_REUSEPORT cannot join the reuseport-owned listener port.
    CHECK_ERR(bind(c, (struct sockaddr *)&addr, sizeof(addr)), EADDRINUSE,
              "plain socket c cannot bind the reuseport listener port");

    close(a);
    close(b);
    close(c);
}

// Contrast: a plain (non-reuseport) listener owns the port exclusively, so a
// later SO_REUSEPORT socket cannot join it. This keeps SO_REUSEPORT from
// becoming a blanket bypass at the listener layer.
static void test_tcp_plain_listener_blocks_reuseport(void) {
    const unsigned short port = 18104;
    struct sockaddr_in addr = loopback(port);

    int a = socket(AF_INET, SOCK_STREAM, 0);
    int b = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(a >= 0 && b >= 0, "create two TCP sockets");

    CHECK_RET(bind(a, (struct sockaddr *)&addr, sizeof(addr)), 0, "plain listener a binds the port");
    CHECK_RET(listen(a, 16), 0, "plain listener a listens on the port");

    CHECK_RET(set_reuseport(b, 1), 0, "socket b: SO_REUSEPORT=1");
    CHECK_ERR(bind(b, (struct sockaddr *)&addr, sizeof(addr)), EADDRINUSE,
              "reuseport socket b cannot join a plain listener's port");

    close(a);
    close(b);
}

// Two UDP sockets that both request SO_REUSEPORT may share the bound port; a
// third socket without SO_REUSEPORT is refused with EADDRINUSE.
static void test_udp_reuseport_group(void) {
    const unsigned short port = 18101;
    struct sockaddr_in addr = loopback(port);

    int a = socket(AF_INET, SOCK_DGRAM, 0);
    int b = socket(AF_INET, SOCK_DGRAM, 0);
    int c = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(a >= 0 && b >= 0 && c >= 0, "create three UDP sockets");

    CHECK_RET(set_reuseport(a, 1), 0, "udp socket a: SO_REUSEPORT=1");
    CHECK_RET(get_reuseport(a), 1, "udp socket a: getsockopt reads 1");
    CHECK_RET(bind(a, (struct sockaddr *)&addr, sizeof(addr)), 0, "udp socket a binds the port");

    CHECK_RET(set_reuseport(b, 1), 0, "udp socket b: SO_REUSEPORT=1");
    CHECK_RET(bind(b, (struct sockaddr *)&addr, sizeof(addr)), 0,
              "udp socket b joins the reuseport group on the same port");

    // No SO_REUSEPORT: must not be allowed to steal the reuseport-owned port.
    CHECK_ERR(bind(c, (struct sockaddr *)&addr, sizeof(addr)), EADDRINUSE,
              "plain udp socket c cannot bind the reuseport-owned port");

    close(a);
    close(b);
    close(c);
}

// Plain UDP double-bind on the same port is rejected once nobody requested
// SO_REUSEPORT (guards against reuseport becoming a blanket bind bypass).
static void test_udp_plain_bind_still_conflicts(void) {
    const unsigned short port = 18102;
    struct sockaddr_in addr = loopback(port);

    int a = socket(AF_INET, SOCK_DGRAM, 0);
    int b = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(a >= 0 && b >= 0, "create two UDP sockets");

    CHECK_RET(bind(a, (struct sockaddr *)&addr, sizeof(addr)), 0, "udp socket a binds the port");
    CHECK_ERR(bind(b, (struct sockaddr *)&addr, sizeof(addr)), EADDRINUSE,
              "udp socket b without SO_REUSEPORT is refused");

    close(a);
    close(b);
}

int main(void) {
    TEST_START("so-reuseport");

    test_option_roundtrip();
    test_tcp_reuseport_group();
    test_plain_bind_still_conflicts();
    test_tcp_reuseport_listen_group();
    test_tcp_plain_listener_blocks_reuseport();
    test_udp_reuseport_group();
    test_udp_plain_bind_still_conflicts();

    TEST_DONE();
}
