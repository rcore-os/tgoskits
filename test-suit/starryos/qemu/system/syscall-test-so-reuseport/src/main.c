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
#include <pthread.h>
#include <stdatomic.h>
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
// plain third socket is refused. Closing one group member keeps the port owned,
// while closing the final member releases it.
static void test_udp_reuseport_group(void) {
    const unsigned short port = 18101;
    struct sockaddr_in addr = loopback(port);

    int a = socket(AF_INET, SOCK_DGRAM, 0);
    int b = socket(AF_INET, SOCK_DGRAM, 0);
    int c = socket(AF_INET, SOCK_DGRAM, 0);
    int d = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(a >= 0 && b >= 0 && c >= 0 && d >= 0, "create four UDP sockets");

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
    CHECK_ERR(bind(d, (struct sockaddr *)&addr, sizeof(addr)), EADDRINUSE,
              "closing one udp reuseport member keeps the port owned");
    close(b);
    CHECK_RET(bind(d, (struct sockaddr *)&addr, sizeof(addr)), 0,
              "closing the final udp reuseport member releases the port");
    close(d);
    close(c);
}

enum {
    CONCURRENT_UDP_PORT = 18105,
    CONCURRENT_ROUNDS = 16,
    CONCURRENT_ERR_PLAIN_SUCCEEDED = -1000,
    CONCURRENT_ERR_WORK_FINISHED = -1001,
};

struct concurrent_udp_state {
    pthread_barrier_t round_start;
    pthread_barrier_t binds_done;
    pthread_barrier_t observer_release;
    atomic_int joiner_error;
    atomic_int plain_error;
    atomic_int observer_progress;
    atomic_int workers_finished;
};

static void concurrent_record_error(atomic_int *slot, int error) {
    int expected = 0;
    (void)atomic_compare_exchange_strong(slot, &expected, error);
}

static int concurrent_state_init(struct concurrent_udp_state *state) {
    int status = pthread_barrier_init(&state->round_start, NULL, 3);
    if (status != 0) {
        return status;
    }
    status = pthread_barrier_init(&state->binds_done, NULL, 3);
    if (status != 0) {
        (void)pthread_barrier_destroy(&state->round_start);
        return status;
    }
    status = pthread_barrier_init(&state->observer_release, NULL, 3);
    if (status != 0) {
        (void)pthread_barrier_destroy(&state->binds_done);
        (void)pthread_barrier_destroy(&state->round_start);
    }
    return status;
}

static void concurrent_state_destroy(struct concurrent_udp_state *state) {
    (void)pthread_barrier_destroy(&state->observer_release);
    (void)pthread_barrier_destroy(&state->binds_done);
    (void)pthread_barrier_destroy(&state->round_start);
}

static void concurrent_rendezvous(pthread_barrier_t *barrier) {
    const int status = pthread_barrier_wait(barrier);
    if (status != 0 && status != PTHREAD_BARRIER_SERIAL_THREAD) {
        _exit(1);
    }
}

static void *concurrent_joiner(void *argument) {
    struct concurrent_udp_state *state = argument;
    const struct sockaddr_in addr = loopback(CONCURRENT_UDP_PORT);
    for (int round = 0; round < CONCURRENT_ROUNDS; round++) {
        concurrent_rendezvous(&state->round_start);

        errno = 0;
        int fd = socket(AF_INET, SOCK_DGRAM, 0);
        if (fd < 0) {
            concurrent_record_error(&state->joiner_error, -errno);
        } else if (set_reuseport(fd, 1) != 0) {
            concurrent_record_error(&state->joiner_error, -errno);
        } else if (bind(fd, (const struct sockaddr *)&addr, sizeof(addr)) != 0) {
            concurrent_record_error(&state->joiner_error, -errno);
        }

        concurrent_rendezvous(&state->binds_done);
        concurrent_rendezvous(&state->observer_release);
        if (fd >= 0) {
            close(fd);
        }
    }

    (void)atomic_fetch_add_explicit(
        &state->workers_finished, 1, memory_order_release);
    return NULL;
}

static void *concurrent_plain_binder(void *argument) {
    struct concurrent_udp_state *state = argument;
    const struct sockaddr_in addr = loopback(CONCURRENT_UDP_PORT);
    for (int round = 0; round < CONCURRENT_ROUNDS; round++) {
        concurrent_rendezvous(&state->round_start);

        errno = 0;
        int fd = socket(AF_INET, SOCK_DGRAM, 0);
        if (fd < 0) {
            concurrent_record_error(&state->plain_error, -errno);
        } else {
            const int bind_status = bind(fd, (const struct sockaddr *)&addr, sizeof(addr));
            const int bind_error = errno;
            if (bind_status == 0) {
                concurrent_record_error(
                    &state->plain_error, CONCURRENT_ERR_PLAIN_SUCCEEDED);
            } else if (bind_error != EADDRINUSE) {
                concurrent_record_error(&state->plain_error, -bind_error);
            }
        }

        concurrent_rendezvous(&state->binds_done);
        concurrent_rendezvous(&state->observer_release);
        if (fd >= 0) {
            close(fd);
        }
    }

    (void)atomic_fetch_add_explicit(
        &state->workers_finished, 1, memory_order_release);
    return NULL;
}

// The bind workers and observer meet at explicit barriers. Both bind attempts
// complete before the observer reports progress, and the workers cannot close
// or advance until that progress has been published. Threads are not pinned;
// this checks task interleaving, not hardware lock contention or latency.
static void test_udp_reuseport_concurrent_progress(void) {
    const struct sockaddr_in addr = loopback(CONCURRENT_UDP_PORT);
    struct concurrent_udp_state state;
    pthread_t joiner;
    pthread_t plain;
    int observer_error = 0;

    const int owner = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(owner >= 0, "create concurrent UDP reuseport owner");
    if (owner < 0) {
        return;
    }
    if (set_reuseport(owner, 1) != 0) {
        CHECK(0, "enable SO_REUSEPORT on concurrent UDP owner");
        close(owner);
        return;
    }
    if (bind(owner, (const struct sockaddr *)&addr, sizeof(addr)) != 0) {
        CHECK(0, "bind concurrent UDP reuseport owner");
        close(owner);
        return;
    }

    atomic_init(&state.joiner_error, 0);
    atomic_init(&state.plain_error, 0);
    atomic_init(&state.observer_progress, 0);
    atomic_init(&state.workers_finished, 0);
    const int init_status = concurrent_state_init(&state);
    CHECK(init_status == 0, "initialize concurrent UDP synchronization");
    if (init_status != 0) {
        close(owner);
        return;
    }

    int status = pthread_create(&joiner, NULL, concurrent_joiner, &state);
    if (status != 0) {
        errno = status;
        CHECK(0, "create concurrent reuseport joiner");
        concurrent_state_destroy(&state);
        close(owner);
        return;
    }
    status = pthread_create(&plain, NULL, concurrent_plain_binder, &state);
    if (status != 0) {
        fprintf(stderr, "concurrent reuseport plain worker create failed: %d\n", status);
        _exit(1);
    }

    for (int round = 0; round < CONCURRENT_ROUNDS; round++) {
        concurrent_rendezvous(&state.round_start);
        concurrent_rendezvous(&state.binds_done);
        if (atomic_load_explicit(
                &state.workers_finished, memory_order_acquire) != 0) {
            observer_error = CONCURRENT_ERR_WORK_FINISHED;
        }
        (void)atomic_fetch_add_explicit(
            &state.observer_progress, 1, memory_order_release);
        concurrent_rendezvous(&state.observer_release);
    }

    int join_status = pthread_join(joiner, NULL);
    if (join_status != 0) {
        fprintf(stderr, "concurrent reuseport joiner join failed: %d\n", join_status);
        _exit(1);
    }
    join_status = pthread_join(plain, NULL);
    if (join_status != 0) {
        fprintf(stderr, "concurrent reuseport plain join failed: %d\n", join_status);
        _exit(1);
    }

    const int joiner_error = atomic_load_explicit(
        &state.joiner_error, memory_order_acquire);
    const int plain_error = atomic_load_explicit(
        &state.plain_error, memory_order_acquire);
    const int observer_progress = atomic_load_explicit(
        &state.observer_progress, memory_order_acquire);
    const int workers_finished = atomic_load_explicit(
        &state.workers_finished, memory_order_acquire);

    if (joiner_error != 0) {
        fprintf(stderr, "concurrent reuseport joiner error=%d\n", joiner_error);
    }
    if (plain_error != 0) {
        fprintf(stderr, "concurrent reuseport plain error=%d\n", plain_error);
    }
    if (observer_error != 0) {
        fprintf(stderr, "concurrent reuseport observer error=%d\n", observer_error);
    }
    CHECK(joiner_error == 0,
          "concurrent reuseport joins complete without errors");
    CHECK(plain_error == 0,
          "concurrent plain binds complete with EADDRINUSE");
    CHECK(observer_error == 0,
          "observer progresses before the contention workload finishes");
    CHECK(observer_progress == CONCURRENT_ROUNDS,
          "observer completes every synchronized round");
    CHECK(workers_finished == 2,
          "both contention workers finish every round");

    concurrent_state_destroy(&state);
    close(owner);

    int released = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(released >= 0, "create socket after concurrent UDP workload");
    if (released >= 0) {
        CHECK_RET(bind(released, (const struct sockaddr *)&addr, sizeof(addr)), 0,
                  "concurrent UDP group releases the port after workload");
        close(released);
    }
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
    test_udp_reuseport_concurrent_progress();
    test_udp_plain_bind_still_conflicts();

    TEST_DONE();
}
