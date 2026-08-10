#define _GNU_SOURCE
#include "icpc-wire.h"
#include "task3-mlp.h"

#include <arpa/inet.h>
#include <errno.h>
#include <math.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#define PEER_IP "10.0.9.3"
#define SETPOINT 100.0
#define SLOW_LOOP_MS 100
#define SLOW_LOOPS 90
#define RECV_TIMEOUT_MS 2000
#define SETTLE_BAND (SETPOINT * 0.02)

enum loop_mode {
    MODE_AI,
    MODE_FIXED,
    MODE_COMPARE,
};

struct loop_metrics {
    double first_err;
    double final_err;
    double final_y;
    double rmse;
    int settle_loops;
    int settled;
    uint64_t mean_oneway_us;
    uint64_t p99_oneway_us;
    int latency_samples;
};

static uint64_t monotonic_us(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0)
        return 0;
    return (uint64_t)ts.tv_sec * 1000000 + (uint64_t)ts.tv_nsec / 1000;
}

static int send_ctrl(int fd, const struct sockaddr_in *peer, socklen_t peer_len,
                     uint32_t seq, const char *payload)
{
    uint8_t out[ICPC_MAX_FRAME];
    size_t plen = strlen(payload);
    size_t n = icpc_encode(ICPC_TYPE_CTRL_CMD, 0, seq, monotonic_us() * 1000, 0,
                           (const uint8_t *)payload, plen, out, sizeof(out));
    if (n == 0)
        return -1;
    return (int)sendto(fd, out, n, 0, (const struct sockaddr *)peer, peer_len);
}

static int recv_state(int fd, uint32_t seq, char *state, size_t cap, uint64_t *t1_ns,
                      uint64_t *t2_ns)
{
    uint64_t deadline = monotonic_us() + (uint64_t)RECV_TIMEOUT_MS * 1000;

    for (;;) {
        uint64_t now = monotonic_us();
        if (now >= deadline)
            return -1;
        unsigned remaining_ms = (unsigned)((deadline - now) / 1000);
        if (remaining_ms == 0)
            remaining_ms = 1;

        fd_set rfds;
        FD_ZERO(&rfds);
        FD_SET(fd, &rfds);
        struct timeval tv = {
            .tv_sec = (time_t)(remaining_ms / 1000),
            .tv_usec = (suseconds_t)((remaining_ms % 1000) * 1000),
        };
        if (select(fd + 1, &rfds, NULL, NULL, &tv) <= 0)
            return -1;

        uint8_t rx[ICPC_MAX_FRAME];
        struct sockaddr_in from;
        socklen_t from_len = sizeof(from);
        ssize_t n = recvfrom(fd, rx, sizeof(rx), 0, (struct sockaddr *)&from, &from_len);
        if (n <= 0)
            continue;

        icpc_header_t hdr;
        const uint8_t *payload = NULL;
        if (icpc_decode(rx, (size_t)n, &hdr, &payload) < 0)
            continue;
        if (hdr.msg_type != ICPC_TYPE_STATE_REPORT || hdr.seq != seq)
            continue;
        if (hdr.payload_len == 0 || cap == 0)
            return -1;
        size_t copy = hdr.payload_len;
        if (copy >= cap)
            copy = cap - 1;
        memcpy(state, payload, copy);
        state[copy] = '\0';
        *t1_ns = hdr.timestamp_ns;
        *t2_ns = monotonic_us() * 1000;
        return 0;
    }
}

static int parse_err(const char *state, double *err_out)
{
    const char *p = strstr(state, "err=");
    if (!p)
        return -1;
    *err_out = strtod(p + 4, NULL);
    return 0;
}

static int parse_y(const char *state, double *y_out)
{
    const char *p = strstr(state, "y=");
    if (!p)
        return -1;
    *y_out = strtod(p + 2, NULL);
    return 0;
}

static uint64_t percentile_us(uint64_t *samples, int count, int rank, int out_of)
{
    if (count <= 0)
        return 0;
    for (int i = 0; i < count - 1; i++) {
        for (int j = i + 1; j < count; j++) {
            if (samples[j] < samples[i]) {
                uint64_t tmp = samples[i];
                samples[i] = samples[j];
                samples[j] = tmp;
            }
        }
    }
    int index = (count - 1) * rank / out_of;
    return samples[index];
}

static int reset_plant(int fd, const struct sockaddr_in *peer, socklen_t peer_len)
{
    char state[64];
    uint64_t t1_ns = 0;
    uint64_t t2_ns = 0;
    if (send_ctrl(fd, peer, peer_len, 0, "reset=1") < 0)
        return -1;
    return recv_state(fd, 0, state, sizeof(state), &t1_ns, &t2_ns);
}

static int run_slow_loop(int fd, const struct sockaddr_in *peer, socklen_t peer_len,
                         enum loop_mode mode, const char *mode_name,
                         struct loop_metrics *metrics)
{
    double kp = 0.5;
    double ki = 0.08;
    double kd = 0.02;
    char payload[128];
    char state[128];
    double err_sum_sq = 0.0;
    int err_samples = 0;
    uint64_t oneway_us[SLOW_LOOPS];
    int oneway_count = 0;

    memset(metrics, 0, sizeof(*metrics));
    metrics->settle_loops = -1;

    printf("TASK3_%s_LOOP_BEGIN\n", mode_name);

    double last_err = 0.0;
    for (int i = 0; i < SLOW_LOOPS; i++) {
        if (mode == MODE_AI) {
            double delta[3];
            task3_mlp_forward(last_err, kp, ki, delta);
            kp += delta[0];
            ki += delta[1];
            kd += delta[2];
            if (kp > 8.0)
                kp = 8.0;
            if (ki > 0.5)
                ki = 0.5;
            if (kd > 1.0)
                kd = 1.0;
        }

        snprintf(payload, sizeof(payload),
                 "kp=%.3f,ki=%.3f,kd=%.3f,setpoint=%.1f", kp, ki, kd, SETPOINT);
        uint32_t seq = (uint32_t)(100 + i);
        uint64_t t1_ns = 0;
        uint64_t t2_ns = 0;
        if (send_ctrl(fd, peer, peer_len, seq, payload) < 0)
            return -1;
        if (recv_state(fd, seq, state, sizeof(state), &t1_ns, &t2_ns) != 0)
            return -1;

        uint64_t rtt_us = (t2_ns - t1_ns) / 1000;
        uint64_t oneway = rtt_us / 2;
        if (oneway_count < SLOW_LOOPS)
            oneway_us[oneway_count++] = oneway;

        double err = 0.0;
        if (parse_err(state, &err) != 0)
            return -1;
        parse_y(state, &metrics->final_y);
        if (i == 0)
            metrics->first_err = err;
        last_err = err;
        err_sum_sq += err * err;
        err_samples++;

        if (metrics->settle_loops < 0 && fabs(err) <= SETTLE_BAND)
            metrics->settle_loops = i;
        if (fabs(err) < SETTLE_BAND && i > 10)
            metrics->settled = 1;

        metrics->final_err = err;
        printf(
            "TASK3_LOOP mode=%s i=%d t1_ns=%llu t2_ns=%llu oneway_us=%llu err=%.3f kp=%.3f state=%s\n",
            mode_name, i, (unsigned long long)t1_ns, (unsigned long long)t2_ns,
            (unsigned long long)oneway, err, kp, state);
        usleep(SLOW_LOOP_MS * 1000);
    }

    metrics->rmse =
        err_samples > 0 ? sqrt(err_sum_sq / (double)err_samples) : 0.0;
    metrics->latency_samples = oneway_count;
    if (oneway_count > 0) {
        uint64_t sum = 0;
        for (int i = 0; i < oneway_count; i++)
            sum += oneway_us[i];
        metrics->mean_oneway_us = sum / (uint64_t)oneway_count;
        metrics->p99_oneway_us =
            percentile_us(oneway_us, oneway_count, 99, 100);
    }

    printf(
        "TASK3_METRICS mode=%s first_err=%.3f final_err=%.3f final_y=%.3f rmse=%.3f settle_loops=%d mean_oneway_us=%llu p99_oneway_us=%llu\n",
        mode_name, metrics->first_err, metrics->final_err, metrics->final_y,
        metrics->rmse, metrics->settle_loops, (unsigned long long)metrics->mean_oneway_us,
        (unsigned long long)metrics->p99_oneway_us);
    return 0;
}

static enum loop_mode parse_mode(int argc, char **argv)
{
    if (argc < 2)
        return MODE_AI;
    if (strcmp(argv[1], "fixed") == 0)
        return MODE_FIXED;
    if (strcmp(argv[1], "compare") == 0)
        return MODE_COMPARE;
    return MODE_AI;
}

int main(int argc, char **argv)
{
    enum loop_mode mode = parse_mode(argc, argv);

    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0)
        return 1;

    struct sockaddr_in peer;
    memset(&peer, 0, sizeof(peer));
    peer.sin_family = AF_INET;
    peer.sin_port = htons(ICPC_PORT);
    if (inet_pton(AF_INET, PEER_IP, &peer.sin_addr) != 1) {
        close(fd);
        return 1;
    }

    if (mode == MODE_COMPARE) {
        struct loop_metrics fixed;
        struct loop_metrics ai;

        if (reset_plant(fd, &peer, sizeof(peer)) < 0)
            return 1;
        usleep(100 * 1000);
        if (run_slow_loop(fd, &peer, sizeof(peer), MODE_FIXED, "FIXED", &fixed) != 0)
            return 1;

        if (reset_plant(fd, &peer, sizeof(peer)) < 0)
            return 1;
        usleep(100 * 1000);
        if (run_slow_loop(fd, &peer, sizeof(peer), MODE_AI, "AI", &ai) != 0)
            return 1;

        printf(
            "TASK3_COMPARE fixed_rmse=%.3f ai_rmse=%.3f fixed_settle=%d ai_settle=%d "
            "fixed_final_err=%.3f ai_final_err=%.3f fixed_p99_oneway_us=%llu ai_p99_oneway_us=%llu\n",
            fixed.rmse, ai.rmse, fixed.settle_loops, ai.settle_loops, fixed.final_err,
            ai.final_err, (unsigned long long)fixed.p99_oneway_us,
            (unsigned long long)ai.p99_oneway_us);
        printf("task3-compare pass\n");
        close(fd);
        return 0;
    }

    const char *mode_name = mode == MODE_FIXED ? "FIXED" : "AI";
    if (mode == MODE_AI)
        printf("TASK3_ONNX_LOOP_BEGIN\n");

    struct loop_metrics metrics;
    if (reset_plant(fd, &peer, sizeof(peer)) < 0)
        return 1;
    usleep(100 * 1000);
    if (run_slow_loop(fd, &peer, sizeof(peer), mode, mode_name, &metrics) != 0)
        return 1;

    close(fd);

    if (mode == MODE_AI &&
        (!metrics.settled || fabs(metrics.final_err) > 6.0 || metrics.final_y < 90.0)) {
        fprintf(stderr, "task3-pid-loop: not settled err=%.3f y=%.3f\n",
                metrics.final_err, metrics.final_y);
        return 1;
    }

    if (mode == MODE_AI)
        printf("task3-pid-loop pass\n");
    else
        printf("task3-fixed-loop done\n");
    return 0;
}
