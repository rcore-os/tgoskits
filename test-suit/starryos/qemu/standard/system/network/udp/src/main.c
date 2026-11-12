#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

/* Ports occupied right after the allocator's last pick in each round. */
#define BUSY_PORTS 256
#define EPHEMERAL_ROUNDS 16
#define CHURN_THREADS 3

static atomic_int stop_churn;
static atomic_ulong churn_cycles;

static int bind_any(int fd, unsigned short port)
{
    struct sockaddr_in addr = {
        .sin_family = AF_INET,
        .sin_port = htons(port),
        .sin_addr = { .s_addr = htonl(INADDR_ANY) },
    };
    return bind(fd, (struct sockaddr *)&addr, sizeof(addr));
}

static int local_port(int fd)
{
    struct sockaddr_in addr;
    socklen_t len = sizeof(addr);
    if (getsockname(fd, (struct sockaddr *)&addr, &len) < 0) {
        return -1;
    }
    return ntohs(addr.sin_port);
}

/* Every close removes a socket from the UDP bind table while ports are allocated. */
static void *churn_sockets(void *arg)
{
    (void)arg;
    while (!atomic_load(&stop_churn)) {
        int fd = socket(AF_INET, SOCK_DGRAM, 0);
        if (fd >= 0) {
            close(fd);
        }
        atomic_fetch_add(&churn_cycles, 1);
    }
    return NULL;
}

/* Binds a fresh socket to port 0 and returns the chosen port, or -1. */
static int ephemeral_port(int *fd_out)
{
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) {
        printf("ephemeral socket: %s\n", strerror(errno));
        return -1;
    }
    if (bind_any(fd, 0) < 0) {
        printf("ephemeral bind: %s\n", strerror(errno));
        close(fd);
        return -1;
    }
    int port = local_port(fd);
    if (port <= 0) {
        printf("ephemeral getsockname returned port %d\n", port);
        close(fd);
        return -1;
    }
    *fd_out = fd;
    return port;
}

static int ephemeral_round(void)
{
    int busy_fds[BUSY_PORTS];
    int busy_ports[BUSY_PORTS];
    int busy = 0;
    int failed = 0;

    int probe;
    int last = ephemeral_port(&probe);
    if (last < 0) {
        return -1;
    }
    close(probe);

    /* Occupy the ports a sequential allocator would try next, so its next pick
     * has to check them all while other sockets are being closed. */
    for (int port = last + 1; port <= 65535 && busy < BUSY_PORTS; port++) {
        int fd = socket(AF_INET, SOCK_DGRAM, 0);
        if (fd < 0) {
            printf("busy socket: %s\n", strerror(errno));
            failed = 1;
            break;
        }
        if (bind_any(fd, (unsigned short)port) < 0) {
            int err = errno;
            close(fd);
            if (err == EADDRINUSE) {
                continue;
            }
            printf("bind busy port %d: %s\n", port, strerror(err));
            failed = 1;
            break;
        }
        busy_fds[busy] = fd;
        busy_ports[busy] = port;
        busy++;
    }

    if (!failed) {
        int fd;
        int port = ephemeral_port(&fd);
        if (port < 0) {
            failed = 1;
        } else {
            for (int i = 0; i < busy; i++) {
                if (busy_ports[i] == port) {
                    printf("bind(0) returned port %d that another socket holds\n", port);
                    failed = 1;
                }
            }
            close(fd);
        }
    }

    for (int i = 0; i < busy; i++) {
        close(busy_fds[i]);
    }
    return failed ? -1 : 0;
}

/* bind(0) must return a port no other socket holds, also while sockets close. */
static int ephemeral_bind_skips_busy_ports(void)
{
    pthread_t threads[CHURN_THREADS];
    int started = 0;
    int failed = 0;

    for (; started < CHURN_THREADS; started++) {
        if (pthread_create(&threads[started], NULL, churn_sockets, NULL) != 0) {
            printf("pthread_create failed\n");
            failed = 1;
            break;
        }
    }
    for (int round = 0; !failed && round < EPHEMERAL_ROUNDS; round++) {
        if (ephemeral_round() < 0) {
            printf("ephemeral round %d failed\n", round);
            failed = 1;
        }
    }
    atomic_store(&stop_churn, 1);
    for (int i = 0; i < started; i++) {
        pthread_join(threads[i], NULL);
    }
    printf("UDP ephemeral rounds=%d busy_ports=%d churn_cycles=%lu\n", EPHEMERAL_ROUNDS,
           BUSY_PORTS, atomic_load(&churn_cycles));
    return failed ? -1 : 0;
}

static int loopback_round_trip(void)
{
    int server = socket(AF_INET, SOCK_DGRAM, 0);
    int client = socket(AF_INET, SOCK_DGRAM, 0);
    if (server < 0 || client < 0) {
        return -1;
    }

    struct sockaddr_in bind_addr = {
        .sin_family = AF_INET,
        .sin_port = 0,
        .sin_addr = { .s_addr = htonl(INADDR_ANY) },
    };
    if (bind(server, (struct sockaddr *)&bind_addr, sizeof(bind_addr)) < 0) {
        return -1;
    }

    socklen_t bind_len = sizeof(bind_addr);
    if (getsockname(server, (struct sockaddr *)&bind_addr, &bind_len) < 0) {
        return -1;
    }

    struct sockaddr_in dst = {
        .sin_family = AF_INET,
        .sin_port = bind_addr.sin_port,
        .sin_addr = { .s_addr = htonl(INADDR_LOOPBACK) },
    };
    const char payload[] = "ping";
    if (sendto(client, payload, sizeof(payload), 0, (struct sockaddr *)&dst, sizeof(dst)) !=
        (ssize_t)sizeof(payload)) {
        return -1;
    }

    char buffer[16] = {0};
    struct sockaddr_in peer = {0};
    socklen_t peer_len = sizeof(peer);
    ssize_t n = recvfrom(server, buffer, sizeof(buffer), 0, (struct sockaddr *)&peer, &peer_len);
    if (n != (ssize_t)sizeof(payload) || memcmp(buffer, payload, sizeof(payload)) != 0) {
        return -1;
    }
    if (peer.sin_family != AF_INET || ntohl(peer.sin_addr.s_addr) != INADDR_LOOPBACK ||
        ntohs(peer.sin_port) == 0) {
        return -1;
    }

    close(client);
    close(server);
    return 0;
}

int main(void)
{
    if (loopback_round_trip() < 0 || ephemeral_bind_skips_busy_ports() < 0) {
        printf("UDP_TEST_FAILED\n");
        return 1;
    }
    printf("UDP_TEST_DONE\n");
    return 0;
}
