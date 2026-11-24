#define _POSIX_C_SOURCE 200809L

#include <arpa/inet.h>
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

enum {
    HOST_PORT = 18383,
    EXPECTED_BODY_SIZE = 4 * 1024 * 1024,
    EXPECTED_BODY_BYTE = 69,
    CONNECT_ATTEMPTS = 100,
};

static void fail(const char *operation)
{
    fprintf(stderr, "E1000_NAPI_TEST_FAILED: %s: %s\n", operation, strerror(errno));
    exit(EXIT_FAILURE);
}

static int connect_host(void)
{
    struct sockaddr_in address = {
        .sin_family = AF_INET,
        .sin_port = htons(HOST_PORT),
    };
    struct timespec retry_delay = {
        .tv_sec = 0,
        .tv_nsec = 100 * 1000 * 1000,
    };
    if (inet_pton(AF_INET, "10.0.2.2", &address.sin_addr) != 1) {
        fail("inet_pton");
    }

    for (int attempt = 0; attempt < CONNECT_ATTEMPTS; ++attempt) {
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        if (fd < 0) {
            fail("socket");
        }
        if (connect(fd, (const struct sockaddr *)&address, sizeof(address)) == 0) {
            return fd;
        }
        int error = errno;
        close(fd);
        errno = error;
        if (errno != ECONNREFUSED && errno != ENETUNREACH && errno != EHOSTUNREACH &&
            errno != ETIMEDOUT) {
            fail("connect");
        }
        nanosleep(&retry_delay, NULL);
    }
    fail("connect retries exhausted");
    return -1;
}

static void fetch_and_verify(void)
{
    static const char request[] =
        "GET /e1000-napi-runtime HTTP/1.0\r\nHost: 10.0.2.2\r\nConnection: close\r\n\r\n";
    unsigned char buffer[8192];
    char header[4097];
    size_t header_length = 0;
    size_t body_length = 0;
    int header_complete = 0;
    int fd = connect_host();

    size_t sent = 0;
    while (sent < sizeof(request) - 1) {
        ssize_t count = send(fd, request + sent, sizeof(request) - 1 - sent, 0);
        if (count < 0 && errno == EINTR) {
            continue;
        }
        if (count <= 0) {
            fail("send request");
        }
        sent += (size_t)count;
    }

    for (;;) {
        ssize_t count = recv(fd, buffer, sizeof(buffer), 0);
        if (count < 0 && errno == EINTR) {
            continue;
        }
        if (count < 0) {
            fail("recv response");
        }
        if (count == 0) {
            break;
        }

        size_t offset = 0;
        if (!header_complete) {
            size_t available = sizeof(header) - 1 - header_length;
            size_t copied = (size_t)count < available ? (size_t)count : available;
            memcpy(header + header_length, buffer, copied);
            header_length += copied;
            header[header_length] = '\0';
            char *separator = NULL;
            if (header_length >= 4) {
                separator = strstr(header, "\r\n\r\n");
            }
            if (separator == NULL) {
                if (header_length == sizeof(header) - 1) {
                    errno = EOVERFLOW;
                    fail("HTTP header too large");
                }
                continue;
            }
            header_complete = 1;
            size_t response_header_length = (size_t)(separator - header) + 4;
            if (strncmp(header, "HTTP/1.0 200", 12) != 0 &&
                strncmp(header, "HTTP/1.1 200", 12) != 0) {
                errno = EPROTO;
                fail("HTTP status");
            }
            if (response_header_length < header_length) {
                size_t buffered_body = header_length - response_header_length;
                for (size_t i = 0; i < buffered_body; ++i) {
                    if ((unsigned char)header[response_header_length + i] != EXPECTED_BODY_BYTE) {
                        errno = EBADMSG;
                        fail("HTTP body byte");
                    }
                }
                body_length += buffered_body;
            }
            offset = copied;
        }

        for (size_t i = offset; i < (size_t)count; ++i) {
            if (buffer[i] != EXPECTED_BODY_BYTE) {
                errno = EBADMSG;
                fail("HTTP body byte");
            }
        }
        body_length += (size_t)count - offset;
    }

    if (!header_complete || body_length != EXPECTED_BODY_SIZE) {
        errno = EMSGSIZE;
        fail("HTTP body length");
    }
    if (close(fd) != 0) {
        fail("close");
    }
}

int main(void)
{
    alarm(60);
    pid_t children[2];
    for (size_t i = 0; i < 2; ++i) {
        children[i] = fork();
        if (children[i] < 0) {
            fail("fork");
        }
        if (children[i] == 0) {
            fetch_and_verify();
            _exit(EXIT_SUCCESS);
        }
    }

    for (size_t i = 0; i < 2; ++i) {
        int status = 0;
        if (waitpid(children[i], &status, 0) != children[i]) {
            fail("waitpid");
        }
        if (!WIFEXITED(status) || WEXITSTATUS(status) != EXIT_SUCCESS) {
            errno = EIO;
            fail("concurrent fetch child");
        }
    }

    puts("E1000_NAPI_TEST_PASSED");
    return EXIT_SUCCESS;
}
