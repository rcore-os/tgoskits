#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
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

static int read_int_option(int fd, int option, int *value, socklen_t *length)
{
    *value = -1;
    *length = sizeof(*value);
    errno = 0;
    return getsockopt(fd, SOL_SOCKET, option, value, length);
}

int main(void)
{
    char socket_path[sizeof(((struct sockaddr_un *)0)->sun_path)];
    snprintf(socket_path, sizeof(socket_path),
             "/tmp/starry-unix-listener-%ld.sock", (long)getpid());
    unlink(socket_path);

    printf("=== bugfix-unix-listener-introspection ===\n");

    int listener = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
    expect_true(listener >= 0, "create Unix stream socket");
    if (listener >= 0) {
        struct stat st;
        expect_true(fstat(listener, &st) == 0 && S_ISSOCK(st.st_mode),
                    "fstat reports a socket inode");

        int value;
        socklen_t option_length;
        expect_true(read_int_option(listener, SO_TYPE, &value, &option_length) == 0 &&
                        value == SOCK_STREAM &&
                        option_length == sizeof(value),
                    "SO_TYPE reports SOCK_STREAM");
        expect_true(read_int_option(listener, SO_ACCEPTCONN, &value,
                                    &option_length) == 0 &&
                        value == 0 && option_length == sizeof(value),
                    "SO_ACCEPTCONN is zero before listen");

        struct sockaddr_un address = { .sun_family = AF_UNIX };
        size_t socket_path_length = strlen(socket_path);
        expect_true(socket_path_length < sizeof(address.sun_path),
                    "socket pathname fits sockaddr_un");
        memcpy(address.sun_path, socket_path, socket_path_length + 1);
        socklen_t address_length =
            offsetof(struct sockaddr_un, sun_path) + socket_path_length + 1;
        expect_true(bind(listener, (struct sockaddr *)&address, address_length) == 0,
                    "bind pathname Unix socket");

        expect_true(read_int_option(listener, SO_ACCEPTCONN, &value,
                                    &option_length) == 0 &&
                        value == 0 && option_length == sizeof(value),
                    "SO_ACCEPTCONN stays zero after bind");

        int early_client = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
        expect_true(early_client >= 0, "create pre-listen client");
        if (early_client >= 0) {
            errno = 0;
            int connect_result =
                connect(early_client, (struct sockaddr *)&address, address_length);
            expect_true(connect_result == -1 && errno == ECONNREFUSED,
                        "connect is refused before listen");
            close(early_client);
        }

        expect_true(listen(listener, 8) == 0, "listen on Unix stream socket");

        expect_true(read_int_option(listener, SO_ACCEPTCONN, &value,
                                    &option_length) == 0 &&
                        value == 1 && option_length == sizeof(value),
                    "SO_ACCEPTCONN is one after listen");

        struct sockaddr_un local = {0};
        socklen_t local_length = sizeof(local);
        expect_true(getsockname(listener, (struct sockaddr *)&local,
                                &local_length) == 0,
                    "getsockname on Unix listener");
        expect_true(local.sun_family == AF_UNIX,
                    "getsockname reports AF_UNIX");
        expect_true(local_length == address_length,
                    "getsockname reports exact pathname length");
        expect_true(strcmp(local.sun_path, socket_path) == 0,
                    "getsockname reports bound pathname");

        int duplicate = dup(listener);
        expect_true(duplicate >= 0, "duplicate listener fd");
        if (duplicate >= 0) {
            expect_true(read_int_option(duplicate, SO_ACCEPTCONN, &value,
                                        &option_length) == 0 &&
                            value == 1 && option_length == sizeof(value),
                        "duplicated fd remains a listening socket");
            close(duplicate);
        }
        close(listener);
    }
    unlink(socket_path);

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("STARRY_UNIX_LISTENER_INTROSPECTION_PASSED\n");
        printf("STARRY_GROUPED_TEST_PASSED: bugfix-unix-listener-introspection\n");
        return EXIT_SUCCESS;
    }
    printf("STARRY_GROUPED_TEST_FAILED: bugfix-unix-listener-introspection\n");
    return EXIT_FAILURE;
}
