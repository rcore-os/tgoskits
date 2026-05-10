#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

static int passed;
static int failed;

#define CHECK(cond, msg) \
    do { \
        if (cond) { \
            printf("  [OK] %s\n", (msg)); \
            passed++; \
        } else { \
            printf("  [FAIL] %s (errno=%d %s)\n", (msg), errno, strerror(errno)); \
            failed++; \
        } \
    } while (0)

static int write_all(int fd, const char *data)
{
    size_t len = strlen(data);
    ssize_t written = write(fd, data, len);
    return written == (ssize_t)len ? 0 : -1;
}

static int create_test_file(const char *path)
{
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) {
        return -1;
    }
    if (write_all(fd, "mmap-fd-zero") != 0) {
        int saved = errno;
        close(fd);
        errno = saved;
        return -1;
    }
    if (lseek(fd, 0, SEEK_SET) != 0) {
        int saved = errno;
        close(fd);
        errno = saved;
        return -1;
    }
    return fd;
}

int main(void)
{
    const char *path = "/tmp/bug_mmap_fd_zero_anon_file";
    const size_t len = 4096;

    printf("=== bug-mmap-fd-zero-anon ===\n");
    unlink(path);

    int fd = create_test_file(path);
    CHECK(fd >= 0, "create mmap source file");
    if (fd < 0) {
        return EXIT_FAILURE;
    }

    int saved_stdin = dup(STDIN_FILENO);
    CHECK(saved_stdin >= 0, "save original stdin fd");
    CHECK(dup2(fd, STDIN_FILENO) == STDIN_FILENO, "move source file onto fd 0");

    errno = 0;
    void *p = mmap(NULL, len, PROT_READ, MAP_PRIVATE, STDIN_FILENO, 0);
    CHECK(p != MAP_FAILED, "file-backed mmap accepts fd 0");
    if (p != MAP_FAILED) {
        CHECK(memcmp(p, "mmap-fd-zero", strlen("mmap-fd-zero")) == 0,
              "fd 0 mapping exposes file contents");
        CHECK(munmap(p, len) == 0, "munmap fd 0 file mapping");
    }

    if (saved_stdin >= 0) {
        CHECK(dup2(saved_stdin, STDIN_FILENO) == STDIN_FILENO, "restore stdin fd");
        close(saved_stdin);
    }

    errno = 0;
    p = mmap(NULL, len, PROT_READ | PROT_WRITE,
             MAP_PRIVATE | MAP_ANONYMOUS, fd, 4096);
    CHECK(p != MAP_FAILED, "anonymous mmap ignores positive fd and accepts page-aligned offset");
    if (p != MAP_FAILED) {
        char *bytes = (char *)p;
        bytes[0] = 'x';
        CHECK(bytes[0] == 'x', "anonymous mapping is writable");
        CHECK(munmap(p, len) == 0, "munmap anonymous mapping");
    }

    errno = 0;
    p = mmap(NULL, len, PROT_READ, MAP_PRIVATE, -1, 0);
    CHECK(p == MAP_FAILED && errno == EBADF,
          "file-backed mmap with fd -1 fails with EBADF");

    errno = 0;
    p = mmap(NULL, len, PROT_READ | PROT_WRITE,
             MAP_PRIVATE | MAP_ANONYMOUS, -1, 1);
    CHECK(p == MAP_FAILED && errno == EINVAL,
          "anonymous mmap still rejects unaligned offset");

    close(fd);
    unlink(path);

    printf("\n=== result: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
    } else {
        printf("TEST FAILED\n");
    }
    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
