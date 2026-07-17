// Migrated from the former nix-sandbox-debug suite.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static char *read_environ(size_t *out_len)
{
    int fd = open("/proc/self/environ", O_RDONLY);
    if (fd < 0) {
        fprintf(stderr, "FAIL: open(/proc/self/environ): %s\n", strerror(errno));
        return NULL;
    }
    char *buf = malloc(8192);
    if (!buf) {
        close(fd);
        return NULL;
    }
    ssize_t total = 0;
    while (total < 8191) {
        ssize_t n = read(fd, buf + total, 8191 - total);
        if (n < 0) {
            if (errno == EINTR) continue;
            fprintf(stderr, "FAIL: read: %s\n", strerror(errno));
            free(buf);
            close(fd);
            return NULL;
        }
        if (n == 0) break;
        total += n;
    }
    close(fd);
    buf[total] = '\0';
    *out_len = (size_t)total;
    return buf;
}

static int find_env(const char *buf, size_t len, const char *var)
{
    size_t varlen = strlen(var);
    const char *p = buf;
    const char *end = buf + len;
    while (p < end) {
        const char *nul = memchr(p, '\0', (size_t)(end - p));
        if (!nul) break;
        size_t entry_len = (size_t)(nul - p);
        if (entry_len == varlen && memcmp(p, var, varlen) == 0) {
            return 1;
        }
        p = nul + 1;
    }
    return 0;
}

static int count_entries(const char *buf, size_t len)
{
    int count = 0;
    const char *p = buf;
    const char *end = buf + len;
    while (p < end) {
        const char *nul = memchr(p, '\0', (size_t)(end - p));
        if (!nul) break;
        size_t entry_len = (size_t)(nul - p);
        if (entry_len > 0) {
            if (memchr(p, '=', entry_len) == NULL) {
                fprintf(stderr, "FAIL: entry without '=': %.*s\n",
                        (int)entry_len, p);
                return -1;
            }
            count++;
        }
        p = nul + 1;
    }
    return count;
}

int main(int argc, char **argv)
{
    if (argc < 2 || strcmp(argv[1], "--verify") != 0) {
        char *new_envp[] = {
            "TEST_ENV=hello123",
            "PATH=/bin",
            "EMPTY_VAL=",
            NULL,
        };
        char *new_argv[] = { argv[0], "--verify", NULL };
        execve("/usr/bin/starry-test-suit/test-proc-environ", new_argv, new_envp);
        fprintf(stderr, "FAIL: execve: %s\n", strerror(errno));
        return 1;
    }

    size_t len = 0;
    char *buf = read_environ(&len);
    if (!buf) return 1;

    if (len == 0) {
        fprintf(stderr, "FAIL: /proc/self/environ is empty\n");
        free(buf);
        return 1;
    }

    printf("INFO: /proc/self/environ size=%zu\n", len);

    if (buf[len - 1] != '\0') {
        fprintf(stderr, "FAIL: environ not NUL-terminated\n");
        free(buf);
        return 1;
    }

    if (!find_env(buf, len, "TEST_ENV=hello123")) {
        fprintf(stderr, "FAIL: TEST_ENV=hello123 not found\n");
        free(buf);
        return 1;
    }
    if (!find_env(buf, len, "PATH=/bin")) {
        fprintf(stderr, "FAIL: PATH=/bin not found\n");
        free(buf);
        return 1;
    }
    if (!find_env(buf, len, "EMPTY_VAL=")) {
        fprintf(stderr, "FAIL: EMPTY_VAL= not found\n");
        free(buf);
        return 1;
    }

    int count = count_entries(buf, len);
    if (count < 0) {
        free(buf);
        return 1;
    }
    printf("INFO: found %d env entries\n", count);

    if (count != 3) {
        fprintf(stderr, "FAIL: expected 3 env entries, got %d\n", count);
        free(buf);
        return 1;
    }

    printf("TEST_PROC_ENVIRON_PASSED\n");
    free(buf);
    return 0;
}
