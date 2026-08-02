#define _GNU_SOURCE

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int passed;
static int failed;

static void expect_true(int condition, const char *name)
{
    if (condition) {
        printf("PASS: %s\n", name);
        passed++;
        return;
    }
    printf("FAIL: %s: errno=%d (%s)\n", name, errno, strerror(errno));
    failed++;
}

static int read_passcred(int fd, int *value)
{
    socklen_t length = sizeof(*value);
    *value = -1;
    errno = 0;
    int result = getsockopt(fd, SOL_SOCKET, SO_PASSCRED, value, &length);
    if (result == 0 && length != sizeof(*value)) {
        errno = EPROTO;
        return -1;
    }
    return result;
}

struct received_message {
    ssize_t length;
    int flags;
    size_t control_length;
    int has_credentials;
    struct ucred credentials;
};

static struct received_message receive_message(int fd, int flags,
                                               int provide_control)
{
    char payload[32] = {0};
    struct iovec iov = {
        .iov_base = payload,
        .iov_len = sizeof(payload),
    };
    union {
        struct cmsghdr align;
        unsigned char bytes[CMSG_SPACE(sizeof(struct ucred))];
    } control = {0};
    struct msghdr message = {
        .msg_iov = &iov,
        .msg_iovlen = 1,
        .msg_control = provide_control ? control.bytes : NULL,
        .msg_controllen = provide_control ? sizeof(control.bytes) : 0,
    };
    struct received_message received = {
        .length = recvmsg(fd, &message, flags),
        .flags = message.msg_flags,
        .control_length = message.msg_controllen,
    };
    if (received.length < 0) {
        return received;
    }

    struct cmsghdr *cmsg = CMSG_FIRSTHDR(&message);
    if (cmsg != NULL && cmsg->cmsg_level == SOL_SOCKET &&
        cmsg->cmsg_type == SCM_CREDENTIALS &&
        cmsg->cmsg_len == CMSG_LEN(sizeof(struct ucred))) {
        memcpy(&received.credentials, CMSG_DATA(cmsg),
               sizeof(received.credentials));
        received.has_credentials = 1;
    }
    return received;
}

static int send_datagram(int fd, const char *payload)
{
    size_t length = strlen(payload);
    return send(fd, payload, length, 0) == (ssize_t)length ? 0 : -1;
}

static int write_datagram(int fd, const char *payload)
{
    size_t length = strlen(payload);
    return write(fd, payload, length) == (ssize_t)length ? 0 : -1;
}

int main(void)
{
    printf("=== bugfix-unix-passcred ===\n");

    int sockets[2];
    expect_true(socketpair(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0, sockets) == 0,
                "create Unix datagram socketpair");
    if (failed != 0) {
        goto finish;
    }

    int enabled;
    expect_true(read_passcred(sockets[0], &enabled) == 0 && enabled == 0,
                "SO_PASSCRED is disabled initially");

    enabled = 1;
    expect_true(setsockopt(sockets[0], SOL_SOCKET, SO_PASSCRED, &enabled,
                           sizeof(enabled)) == 0,
                "enable SO_PASSCRED on receiver");
    expect_true(read_passcred(sockets[0], &enabled) == 0 && enabled == 1,
                "SO_PASSCRED enable state reads back");

    pid_t child = fork();
    expect_true(child >= 0, "fork credential sender");
    if (child == 0) {
        close(sockets[0]);
        int result = send_datagram(sockets[1], "first") |
                     send_datagram(sockets[1], "peek") |
                     send_datagram(sockets[1], "truncate") |
                     write_datagram(sockets[1], "write");
        close(sockets[1]);
        _exit(result == 0 ? EXIT_SUCCESS : EXIT_FAILURE);
    }
    if (child < 0) {
        close(sockets[0]);
        close(sockets[1]);
        goto finish;
    }

    close(sockets[1]);

    struct received_message first = receive_message(sockets[0], 0, 1);
    expect_true(first.length == 5, "receive first child datagram");
    expect_true(first.has_credentials, "receive SCM_CREDENTIALS automatically");
    expect_true(first.has_credentials && first.credentials.pid == child,
                "SCM_CREDENTIALS reports sending child PID");
    expect_true(first.has_credentials && first.credentials.uid == getuid(),
                "SCM_CREDENTIALS reports sender real UID");
    expect_true(first.has_credentials && first.credentials.gid == getgid(),
                "SCM_CREDENTIALS reports sender real GID");

    struct received_message peeked =
        receive_message(sockets[0], MSG_PEEK, 1);
    expect_true(peeked.length == 4 && peeked.has_credentials,
                "MSG_PEEK reports credentials");
    expect_true(peeked.has_credentials && peeked.credentials.pid == child,
                "MSG_PEEK preserves sender PID");

    struct received_message consumed = receive_message(sockets[0], 0, 1);
    expect_true(consumed.length == 4 && consumed.has_credentials,
                "consume after MSG_PEEK reports credentials");
    expect_true(consumed.has_credentials &&
                    consumed.credentials.pid == peeked.credentials.pid &&
                    consumed.credentials.uid == peeked.credentials.uid &&
                    consumed.credentials.gid == peeked.credentials.gid,
                "peek and consume report identical credentials");

    struct received_message truncated = receive_message(sockets[0], 0, 0);
    expect_true(truncated.length == 8, "receive datagram without control buffer");
    expect_true((truncated.flags & MSG_CTRUNC) != 0,
                "missing credential control space sets MSG_CTRUNC");

    struct received_message written = receive_message(sockets[0], 0, 1);
    expect_true(written.length == 5, "receive write(2) datagram");
    expect_true(written.has_credentials,
                "write(2) automatically reports SCM_CREDENTIALS");
    expect_true(written.has_credentials && written.credentials.pid == child,
                "write(2) credentials report sending child PID");

    int status = 0;
    expect_true(waitpid(child, &status, 0) == child &&
                    WIFEXITED(status) && WEXITSTATUS(status) == EXIT_SUCCESS,
                "credential sender exits successfully");

    enabled = 0;
    expect_true(setsockopt(sockets[0], SOL_SOCKET, SO_PASSCRED, &enabled,
                           sizeof(enabled)) == 0,
                "disable SO_PASSCRED on receiver");
    expect_true(read_passcred(sockets[0], &enabled) == 0 && enabled == 0,
                "SO_PASSCRED disable state reads back");
    close(sockets[0]);

    expect_true(socketpair(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0, sockets) == 0,
                "create disabled-state socketpair");
    if (sockets[0] >= 0 && sockets[1] >= 0) {
        expect_true(send_datagram(sockets[1], "disabled") == 0,
                    "send with SO_PASSCRED disabled");
        struct received_message disabled = receive_message(sockets[0], 0, 1);
        expect_true(disabled.length == 8, "receive disabled-state datagram");
        expect_true(!disabled.has_credentials &&
                        (disabled.flags & MSG_CTRUNC) == 0 &&
                        disabled.control_length == 0,
                    "disabled SO_PASSCRED emits no credentials");
        close(sockets[0]);
        close(sockets[1]);
    }

finish:
    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("STARRY_UNIX_PASSCRED_PASSED\n");
        printf("STARRY_GROUPED_TEST_PASSED: bugfix-unix-passcred\n");
        return EXIT_SUCCESS;
    }
    printf("STARRY_GROUPED_TEST_FAILED: bugfix-unix-passcred\n");
    return EXIT_FAILURE;
}
