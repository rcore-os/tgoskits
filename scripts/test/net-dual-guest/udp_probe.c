// Small Linux Guest UDP probe for P1/P2 evidence.
//
// `send` waits for the matching ACK before advancing, which makes the pcap
// sequence and ACK counts deterministic. `recv` echoes only PING payloads.

#define _POSIX_C_SOURCE 200809L

#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t stop_requested;

static void stop(int signal_number) {
    (void)signal_number;
    stop_requested = 1;
}

static uint64_t monotonic_ms(void) {
    struct timespec time;
    clock_gettime(CLOCK_MONOTONIC, &time);
    return (uint64_t)time.tv_sec * 1000u + (uint64_t)time.tv_nsec / 1000000u;
}

static int parse_u32(const char *text, uint32_t *value) {
    char *end = NULL;
    unsigned long parsed = strtoul(text, &end, 10);
    if (end == text || *end != '\0' || parsed > UINT32_MAX) {
        return -1;
    }
    *value = (uint32_t)parsed;
    return 0;
}

static int parse_port(const char *text, uint16_t *port) {
    uint32_t value;
    if (parse_u32(text, &value) != 0 || value == 0 || value > 65535) {
        return -1;
    }
    *port = (uint16_t)value;
    return 0;
}

static int run_receiver(uint16_t port) {
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) {
        perror("socket");
        return 1;
    }
    struct sockaddr_in address = {
        .sin_family = AF_INET,
        .sin_addr.s_addr = htonl(INADDR_ANY),
        .sin_port = htons(port),
    };
    if (bind(fd, (const struct sockaddr *)&address, sizeof(address)) < 0) {
        perror("bind");
        close(fd);
        return 1;
    }
    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    printf("TASK2_PROBE_READY port=%u\n", (unsigned)port);
    fflush(stdout);
    while (!stop_requested) {
        char payload[1500];
        struct sockaddr_in source;
        socklen_t source_len = sizeof(source);
        ssize_t length = recvfrom(fd, payload, sizeof(payload) - 1, 0,
                                  (struct sockaddr *)&source, &source_len);
        if (length < 0) {
            if (errno == EINTR) {
                continue;
            }
            perror("recvfrom");
            close(fd);
            return 1;
        }
        payload[length] = '\0';
        printf("TASK2_PROBE_RX bytes=%zd payload=%s\n", length, payload);
        if (length >= 5 && memcmp(payload, "PING ", 5) == 0) {
            char reply[1500];
            int reply_len = snprintf(reply, sizeof(reply), "ACK %s", payload + 5);
            if (reply_len < 0 || (size_t)reply_len >= sizeof(reply)
                || sendto(fd, reply, (size_t)reply_len, 0,
                          (const struct sockaddr *)&source, source_len) < 0) {
                perror("sendto ACK");
                close(fd);
                return 1;
            }
        }
        fflush(stdout);
    }
    close(fd);
    return 0;
}

static int run_sender(const char *address, uint16_t port, uint32_t count,
                      uint32_t interval_ms, const char *tag) {
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) {
        perror("socket");
        return 1;
    }
    struct sockaddr_in destination = {
        .sin_family = AF_INET,
        .sin_port = htons(port),
    };
    if (inet_pton(AF_INET, address, &destination.sin_addr) != 1) {
        fprintf(stderr, "invalid IPv4 address: %s\n", address);
        close(fd);
        return 1;
    }
    uint32_t acknowledged = 0;
    for (uint32_t sequence = 0; sequence < count && !stop_requested; ++sequence) {
        char payload[1500];
        int length = snprintf(payload, sizeof(payload), "PING %u %s %llu", sequence,
                              tag, (unsigned long long)monotonic_ms());
        if (length < 0 || (size_t)length >= sizeof(payload)
            || sendto(fd, payload, (size_t)length, 0,
                      (const struct sockaddr *)&destination, sizeof(destination)) < 0) {
            perror("sendto");
            close(fd);
            return 1;
        }
        struct pollfd pollfd = {.fd = fd, .events = POLLIN};
        int ready = poll(&pollfd, 1, 500);
        if (ready > 0 && (pollfd.revents & POLLIN)) {
            char reply[1500];
            ssize_t reply_len = recv(fd, reply, sizeof(reply) - 1, 0);
            if (reply_len >= 5 && memcmp(reply, "ACK ", 4) == 0) {
                char *end = NULL;
                unsigned long ack_sequence = strtoul(reply + 4, &end, 10);
                if (end != reply + 4 && (uint32_t)ack_sequence == sequence) {
                    ++acknowledged;
                }
            }
        }
        if (interval_ms > 0) {
            struct timespec delay = {
                .tv_sec = (time_t)(interval_ms / 1000),
                .tv_nsec = (long)(interval_ms % 1000) * 1000000L,
            };
            nanosleep(&delay, NULL);
        }
    }
    printf("TASK2_PROBE_SUMMARY sent=%u acked=%u ack_rate=%u%%\n", count,
           acknowledged, count == 0 ? 0 : (acknowledged * 100u) / count);
    close(fd);
    return acknowledged == count ? 0 : 2;
}

int main(int argc, char **argv) {
    if (argc >= 3 && strcmp(argv[1], "recv") == 0) {
        uint16_t port;
        return parse_port(argv[2], &port) == 0 ? run_receiver(port) : 2;
    }
    if (argc >= 5 && strcmp(argv[1], "send") == 0) {
        uint16_t port;
        uint32_t count;
        uint32_t interval = 0;
        if (parse_port(argv[3], &port) != 0 || parse_u32(argv[4], &count) != 0
            || (argc >= 6 && parse_u32(argv[5], &interval) != 0)) {
            return 2;
        }
        return run_sender(argv[2], port, count, interval,
                          argc >= 7 ? argv[6] : "probe");
    }
    fprintf(stderr, "usage: udp_probe recv PORT | send IP PORT COUNT [INTERVAL_MS] [TAG]\n");
    return 2;
}
