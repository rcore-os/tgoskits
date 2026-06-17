#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

enum {
    BUF_SIZE = 65536,
};

static unsigned char buffer[BUF_SIZE];

int main(void)
{
    int zero_fd = open("/dev/zero", O_RDONLY);
    if (zero_fd < 0) {
        printf("TEST FAILED: open /dev/zero: %s\n", strerror(errno));
        return EXIT_FAILURE;
    }

    ssize_t nread = read(zero_fd, buffer, sizeof(buffer));
    close(zero_fd);
    if (nread != (ssize_t)sizeof(buffer)) {
        printf("TEST FAILED: /dev/zero short read got %zd expected %zu\n", nread, sizeof(buffer));
        return EXIT_FAILURE;
    }

    for (size_t i = 0; i < sizeof(buffer); i++) {
        if (buffer[i] != 0) {
            printf("TEST FAILED: /dev/zero byte %zu is %u\n", i, buffer[i]);
            return EXIT_FAILURE;
        }
    }

    int null_fd = open("/dev/null", O_WRONLY);
    if (null_fd < 0) {
        printf("TEST FAILED: open /dev/null: %s\n", strerror(errno));
        return EXIT_FAILURE;
    }

    ssize_t nwritten = write(null_fd, buffer, sizeof(buffer));
    close(null_fd);
    if (nwritten != (ssize_t)sizeof(buffer)) {
        printf("TEST FAILED: /dev/null short write got %zd expected %zu\n", nwritten, sizeof(buffer));
        return EXIT_FAILURE;
    }

    puts("TEST PASSED");
    return EXIT_SUCCESS;
}
