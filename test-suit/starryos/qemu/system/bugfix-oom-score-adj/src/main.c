#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define OOM_SCORE_ADJ_PATH "/proc/self/oom_score_adj"

static int failures;

static void check(int condition, const char *message)
{
    if (condition) {
        printf("PASS: %s\n", message);
        return;
    }

    fprintf(stderr, "FAIL: %s: errno=%d (%s)\n", message, errno,
            strerror(errno));
    failures++;
}

static ssize_t write_oom_score_adj(const char *value)
{
    int fd = open(OOM_SCORE_ADJ_PATH, O_WRONLY);
    if (fd < 0) {
        return -1;
    }

    ssize_t result = write(fd, value, strlen(value));
    int write_errno = errno;
    close(fd);
    errno = write_errno;
    return result;
}

static int read_oom_score_adj(void)
{
    char buffer[32];
    int fd = open(OOM_SCORE_ADJ_PATH, O_RDONLY);
    if (fd < 0) {
        return 2000;
    }

    ssize_t length = read(fd, buffer, sizeof(buffer) - 1);
    int read_errno = errno;
    close(fd);
    errno = read_errno;
    if (length <= 0) {
        return 2000;
    }

    buffer[length] = '\0';
    char *end = NULL;
    long value = strtol(buffer, &end, 10);
    if (end == buffer) {
        errno = EINVAL;
        return 2000;
    }
    return (int)value;
}

int main(void)
{
    const char adjusted[] = "-250\n";

    errno = 0;
    check(write_oom_score_adj(adjusted) == (ssize_t)(sizeof(adjusted) - 1),
          "oom_score_adj accepts a newline-terminated signed value");

    errno = 0;
    check(read_oom_score_adj() == -250,
          "oom_score_adj reports the adjusted value");

    errno = 0;
    check(write_oom_score_adj("1001") == -1 && errno == EINVAL,
          "oom_score_adj rejects values above 1000");

    errno = 0;
    check(write_oom_score_adj("-1001") == -1 && errno == EINVAL,
          "oom_score_adj rejects values below -1000");

    if (failures != 0) {
        fprintf(stderr, "STARRY_OOM_SCORE_ADJ_FAILED: %d checks\n", failures);
        return 1;
    }

    puts("STARRY_OOM_SCORE_ADJ_PASSED");
    return 0;
}
