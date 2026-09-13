#define _GNU_SOURCE

#include <errno.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef TCP_CA_NAME_MAX
#define TCP_CA_NAME_MAX 16
#endif

static int failures;

static void fail(const char *step)
{
    fprintf(stderr, "FAIL: %s: errno=%d (%s)\n", step, errno,
            strerror(errno));
    failures++;
}

int main(void)
{
    char algorithm[TCP_CA_NAME_MAX] = {0};
    socklen_t algorithm_len = sizeof(algorithm);
    int socket_fd = socket(AF_INET, SOCK_STREAM, 0);

    if (socket_fd < 0) {
        fail("socket(AF_INET, SOCK_STREAM)");
        goto out;
    }

    errno = 0;
    if (syscall(SYS_getsockopt, socket_fd, IPPROTO_TCP, TCP_CONGESTION,
                algorithm, &algorithm_len) != 0) {
        fail("getsockopt(TCP_CONGESTION)");
        goto close_socket;
    }
    if (algorithm_len != TCP_CA_NAME_MAX || algorithm[0] == '\0' ||
        memchr(algorithm, '\0', sizeof(algorithm)) == NULL) {
        fprintf(stderr,
                "FAIL: invalid TCP_CONGESTION result: len=%u first=%u\n",
                (unsigned int)algorithm_len, (unsigned int)algorithm[0]);
        failures++;
        goto close_socket;
    }

    unsigned char untouched = 0xa5;
    socklen_t zero_length = 0;
    errno = 0;
    if (syscall(SYS_getsockopt, socket_fd, IPPROTO_TCP, TCP_CONGESTION,
                &untouched, &zero_length) != 0 ||
        zero_length != 0 || untouched != 0xa5) {
        fprintf(stderr,
                "FAIL: zero-length getsockopt(TCP_CONGESTION): errno=%d "
                "len=%u value=%u\n",
                errno, (unsigned int)zero_length, (unsigned int)untouched);
        failures++;
    }

    errno = 0;
    if (syscall(SYS_setsockopt, socket_fd, IPPROTO_TCP, TCP_CONGESTION,
                algorithm, strlen(algorithm)) != 0) {
        fail("setsockopt(current TCP_CONGESTION)");
    }

    errno = 0;
    if (syscall(SYS_setsockopt, socket_fd, IPPROTO_TCP, TCP_CONGESTION,
                "starry-invalid", strlen("starry-invalid")) != -1 ||
        errno != ENOENT) {
        fprintf(stderr,
                "FAIL: unknown TCP_CONGESTION: errno=%d, expected ENOENT\n",
                errno);
        failures++;
    }

    errno = 0;
    if (syscall(SYS_setsockopt, socket_fd, IPPROTO_TCP, TCP_CONGESTION,
                algorithm, 0) != -1 || errno != EINVAL) {
        fprintf(stderr,
                "FAIL: zero-length TCP_CONGESTION: errno=%d, expected EINVAL\n",
                errno);
        failures++;
    }

close_socket:
    close(socket_fd);
out:
    if (failures == 0) {
        printf("TCP_CONGESTION=%s\n", algorithm);
        printf("STARRY_GROUPED_TEST_PASSED: bugfix-tcp-congestion-sockopt\n");
        return EXIT_SUCCESS;
    }

    printf("STARRY_GROUPED_TEST_FAILED: bugfix-tcp-congestion-sockopt\n");
    return EXIT_FAILURE;
}
