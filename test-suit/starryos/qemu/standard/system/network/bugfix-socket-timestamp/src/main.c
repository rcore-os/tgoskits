#define _GNU_SOURCE

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static int passed;
static int failed;

struct timestamp_result {
    int found;
    struct timeval value;
};

static void note_pass(const char *name)
{
    printf("PASS: %s\n", name);
    passed++;
}

static void note_fail(const char *name, const char *detail)
{
    printf("FAIL: %s: %s\n", name, detail);
    failed++;
}

static int expect_true(int condition, const char *name)
{
    if (condition) {
        note_pass(name);
        return 1;
    }
    note_fail(name, "condition is false");
    return 0;
}

static int64_t timespec_to_micros(struct timespec value)
{
    return (int64_t)value.tv_sec * 1000000 + value.tv_nsec / 1000;
}

static int64_t timeval_to_micros(struct timeval value)
{
    return (int64_t)value.tv_sec * 1000000 + value.tv_usec;
}

static void expect_timestamp_option(int fd, int enabled, const char *name)
{
    int value = -1;
    socklen_t value_len = sizeof(value);
    errno = 0;
    if (getsockopt(fd, SOL_SOCKET, SO_TIMESTAMP, &value, &value_len) == 0 &&
        value_len == sizeof(value) && value == enabled) {
        note_pass(name);
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail), "value=%d len=%u errno=%d (%s)", value,
             (unsigned)value_len, errno, strerror(errno));
    note_fail(name, detail);
}

static void set_timestamp_option(int fd, int enabled, const char *name)
{
    errno = 0;
    if (setsockopt(fd, SOL_SOCKET, SO_TIMESTAMP, &enabled,
                   sizeof(enabled)) == 0) {
        note_pass(name);
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail), "errno=%d (%s)", errno, strerror(errno));
    note_fail(name, detail);
}

static int send_byte(int fd, char value, const char *name)
{
    errno = 0;
    ssize_t sent = send(fd, &value, sizeof(value), 0);
    if (sent == (ssize_t)sizeof(value)) {
        note_pass(name);
        return 0;
    }

    char detail[160];
    snprintf(detail, sizeof(detail), "sent=%zd errno=%d (%s)", sent, errno,
             strerror(errno));
    note_fail(name, detail);
    return -1;
}

static int recv_byte_with_timestamp(int fd, int flags, char expected,
                                    struct timestamp_result *result,
                                    const char *name)
{
    char value = '\0';
    char control[CMSG_SPACE(sizeof(struct timeval))] = {0};
    struct iovec iov = {
        .iov_base = &value,
        .iov_len = sizeof(value),
    };
    struct msghdr msg = {
        .msg_iov = &iov,
        .msg_iovlen = 1,
        .msg_control = control,
        .msg_controllen = sizeof(control),
    };

    memset(result, 0, sizeof(*result));
    errno = 0;
    ssize_t received = recvmsg(fd, &msg, flags);
    if (received != (ssize_t)sizeof(value) || value != expected) {
        char detail[160];
        snprintf(detail, sizeof(detail),
                 "received=%zd value=%d errno=%d (%s)", received, value, errno,
                 strerror(errno));
        note_fail(name, detail);
        return -1;
    }
    note_pass(name);

    struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
    if (cmsg != NULL && cmsg->cmsg_level == SOL_SOCKET &&
        cmsg->cmsg_type == SCM_TIMESTAMP &&
        cmsg->cmsg_len == CMSG_LEN(sizeof(struct timeval))) {
        memcpy(&result->value, CMSG_DATA(cmsg), sizeof(result->value));
        result->found = 1;
    }
    return 0;
}

static void expect_no_timestamp(const struct timestamp_result *result,
                                const char *name)
{
    expect_true(!result->found, name);
}

static void test_timestamp_delivery(void)
{
    int sockets[2] = {-1, -1};
    if (!expect_true(socketpair(AF_UNIX, SOCK_DGRAM, 0, sockets) == 0,
                     "create Unix datagram socketpair")) {
        return;
    }

    expect_timestamp_option(sockets[1], 0, "SO_TIMESTAMP defaults to disabled");
    set_timestamp_option(sockets[1], 1, "enable SO_TIMESTAMP");
    expect_timestamp_option(sockets[1], 1, "SO_TIMESTAMP reports enabled");

    struct timespec before_send;
    struct timespec after_send;
    struct timespec before_recv;
    clock_gettime(CLOCK_REALTIME, &before_send);
    send_byte(sockets[0], 'A', "send timestamped datagram");
    clock_gettime(CLOCK_REALTIME, &after_send);
    usleep(100000);
    clock_gettime(CLOCK_REALTIME, &before_recv);

    struct timestamp_result result;
    if (recv_byte_with_timestamp(sockets[1], 0, 'A', &result,
                                 "receive timestamped datagram") == 0) {
        expect_true(result.found, "recvmsg returns SCM_TIMESTAMP");
        if (result.found) {
            int64_t timestamp = timeval_to_micros(result.value);
            int64_t send_start = timespec_to_micros(before_send);
            int64_t send_end = timespec_to_micros(after_send);
            int64_t recv_start = timespec_to_micros(before_recv);
            expect_true(result.value.tv_usec >= 0 &&
                            result.value.tv_usec < 1000000,
                        "SCM_TIMESTAMP contains a valid timeval");
            expect_true(timestamp >= send_start - 1000 &&
                            timestamp <= send_end + 10000,
                        "SCM_TIMESTAMP records the enqueue window");
            expect_true(recv_start - timestamp >= 50000,
                        "SCM_TIMESTAMP is not generated at recvmsg time");
        }
    }

    set_timestamp_option(sockets[1], 0, "disable SO_TIMESTAMP");
    expect_timestamp_option(sockets[1], 0, "SO_TIMESTAMP reports disabled");
    send_byte(sockets[0], 'B', "send datagram while timestamping disabled");
    if (recv_byte_with_timestamp(sockets[1], 0, 'B', &result,
                                 "receive datagram while disabled") == 0) {
        expect_no_timestamp(&result,
                            "disabled SO_TIMESTAMP returns no timestamp cmsg");
    }

    send_byte(sockets[0], 'C', "queue datagram before enabling timestamping");
    set_timestamp_option(sockets[1], 1,
                         "enable SO_TIMESTAMP after datagram was queued");
    struct timespec before_fallback_recv;
    struct timespec after_fallback_recv;
    clock_gettime(CLOCK_REALTIME, &before_fallback_recv);
    if (recv_byte_with_timestamp(sockets[1], 0, 'C', &result,
                                 "receive datagram queued while disabled") == 0) {
        clock_gettime(CLOCK_REALTIME, &after_fallback_recv);
        expect_true(result.found,
                    "enable-after-enqueue returns Linux fallback timestamp");
        if (result.found) {
            int64_t timestamp = timeval_to_micros(result.value);
            int64_t recv_start = timespec_to_micros(before_fallback_recv);
            int64_t recv_end = timespec_to_micros(after_fallback_recv);
            expect_true(timestamp >= recv_start - 1000 &&
                            timestamp <= recv_end + 10000,
                        "enable-after-enqueue fallback uses recvmsg time");
        }
    }

    send_byte(sockets[0], 'D', "queue datagram while timestamping enabled");
    set_timestamp_option(sockets[1], 0,
                         "disable SO_TIMESTAMP before reading queued datagram");
    if (recv_byte_with_timestamp(sockets[1], 0, 'D', &result,
                                 "receive timestamped datagram after disable") ==
        0) {
        expect_no_timestamp(
            &result,
            "disabling before recvmsg suppresses queued timestamp delivery");
    }

    set_timestamp_option(sockets[1], 1,
                         "re-enable SO_TIMESTAMP for MSG_PEEK");
    send_byte(sockets[0], 'E', "send datagram for MSG_PEEK");
    struct timestamp_result peeked;
    struct timestamp_result consumed;
    if (recv_byte_with_timestamp(sockets[1], MSG_PEEK, 'E', &peeked,
                                 "peek timestamped datagram") == 0 &&
        recv_byte_with_timestamp(sockets[1], 0, 'E', &consumed,
                                 "consume timestamped datagram") == 0) {
        expect_true(peeked.found && consumed.found,
                    "peek and consume both return SCM_TIMESTAMP");
        if (peeked.found && consumed.found) {
            expect_true(peeked.value.tv_sec == consumed.value.tv_sec &&
                            peeked.value.tv_usec == consumed.value.tv_usec,
                        "MSG_PEEK preserves the datagram timestamp");
        }
    }

    close(sockets[0]);
    close(sockets[1]);
}

int main(void)
{
    printf("=== bugfix-socket-timestamp ===\n");

    test_timestamp_delivery();

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("STARRY_SOCKET_TIMESTAMP_PASSED\n");
        printf("STARRY_GROUPED_TEST_PASSED: bugfix-socket-timestamp\n");
        return EXIT_SUCCESS;
    }
    printf("STARRY_GROUPED_TEST_FAILED: bugfix-socket-timestamp\n");
    return EXIT_FAILURE;
}
