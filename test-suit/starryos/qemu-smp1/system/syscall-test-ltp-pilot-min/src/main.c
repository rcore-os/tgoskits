/*
 * Minimal dynamic confirmation for LTP-derived syscall conformance fixes.
 *
 * Scope is intentionally narrow:
 * - openat2 open_how validation and ordinary-path fallback through openat
 * - mmap invalid PROT bit validation
 * - multi-component PATH_MAX handling for representative path syscalls
 */

#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_openat2
#define SYS_openat2 437
#endif

#ifndef STATX_BASIC_STATS
#define STATX_BASIC_STATS 0x000007ffU
#endif

#ifndef RESOLVE_NO_XDEV
#define RESOLVE_NO_XDEV        0x01
#define RESOLVE_NO_MAGICLINKS  0x02
#define RESOLVE_NO_SYMLINKS    0x04
#define RESOLVE_BENEATH        0x08
#define RESOLVE_IN_ROOT        0x10
#endif

#ifndef PATH_MAX
#define PATH_MAX 4096
#endif

struct open_how {
    uint64_t flags;
    uint64_t mode;
    uint64_t resolve;
};

struct open_how_pad {
    struct open_how how;
    uint64_t pad;
};

static int pass_count;
static int fail_count;

#define CHECK(cond, msg) do {                                           \
    if (cond) {                                                         \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);      \
        pass_count++;                                                   \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | errno=%d (%s)\n",                \
               __FILE__, __LINE__, msg, errno, strerror(errno));        \
        fail_count++;                                                   \
    }                                                                   \
} while (0)

static long do_openat2(int dirfd, const char *path, const struct open_how *how,
                       size_t size)
{
    return syscall(SYS_openat2, dirfd, path, how, size);
}

static void expect_openat2_errno(const char *name, int dirfd, const char *path,
                                 struct open_how how, size_t size, int expected)
{
    errno = 0;
    long ret = do_openat2(dirfd, path, &how, size);
    if (ret >= 0) {
        close((int)ret);
    }
    CHECK(ret == -1 && errno == expected, name);
}

static void expect_openat2_padded_e2big(void)
{
    struct open_how_pad padded = {
        .how = {
            .flags = O_RDWR | O_CREAT,
            .mode = 0700,
            .resolve = 0,
        },
        .pad = 0xdead,
    };

    errno = 0;
    long ret = do_openat2(AT_FDCWD, "openat2-e2big", &padded.how, sizeof(padded));
    if (ret >= 0) {
        close((int)ret);
    }
    CHECK(ret == -1 && errno == E2BIG, "openat2 nonzero extension bytes -> E2BIG");
}

static void expect_openat2_success(const char *name, int dirfd, const char *path,
                                   uint64_t flags, uint64_t resolve)
{
    struct open_how how = {
        .flags = flags | O_CREAT,
        .mode = 0600,
        .resolve = resolve,
    };
    struct stat st;

    errno = 0;
    long ret = do_openat2(dirfd, path, &how, sizeof(how));
    if (ret < 0) {
        CHECK(0, name);
        return;
    }

    CHECK(fstat((int)ret, &st) == 0 && st.st_size == 0, name);
    close((int)ret);
    unlinkat(dirfd, path, 0);
}

static void test_openat2_min(void)
{
    const char *dir = "openat2-min-dir";
    int dirfd;

    printf("[TEST] openat2 minimal conformance\n");
    unlink("openat2-e2big");
    rmdir(dir);
    CHECK(mkdir(dir, 0700) == 0 || errno == EEXIST, "openat2 setup mkdir");
    dirfd = open(dir, O_RDONLY | O_DIRECTORY);
    CHECK(dirfd >= 0, "openat2 setup open dir");
    if (dirfd < 0) {
        return;
    }

    expect_openat2_errno("openat2 invalid dirfd -> EBADF", -1, "file",
                         (struct open_how){ O_RDWR | O_CREAT, 0700, 0 },
                         sizeof(struct open_how), EBADF);
    expect_openat2_errno("openat2 NULL pathname -> EFAULT", AT_FDCWD, NULL,
                         (struct open_how){ O_RDONLY | O_CREAT, 0400, 0 },
                         sizeof(struct open_how), EFAULT);
    expect_openat2_errno("openat2 mode without create -> EINVAL", AT_FDCWD, "file",
                         (struct open_how){ O_RDONLY, 0200, 0 },
                         sizeof(struct open_how), EINVAL);
    expect_openat2_errno("openat2 invalid mode -> EINVAL", AT_FDCWD, "file",
                         (struct open_how){ O_RDWR | O_CREAT, UINT64_MAX, 0 },
                         sizeof(struct open_how), EINVAL);
    expect_openat2_errno("openat2 invalid resolve -> EINVAL", AT_FDCWD, "file",
                         (struct open_how){ O_RDWR | O_CREAT, 0700, UINT64_MAX },
                         sizeof(struct open_how), EINVAL);
    expect_openat2_errno("openat2 size zero -> EINVAL", AT_FDCWD, "file",
                         (struct open_how){ O_RDWR | O_CREAT, 0700, 0 },
                         0, EINVAL);
    expect_openat2_errno("openat2 size small -> EINVAL", AT_FDCWD, "file",
                         (struct open_how){ O_RDWR | O_CREAT, 0700, 0 },
                         sizeof(struct open_how) - 1, EINVAL);
    expect_openat2_padded_e2big();

    expect_openat2_success("openat2 ordinary path succeeds", dirfd, "basic", O_RDWR, 0);
    expect_openat2_success("openat2 RESOLVE_NO_XDEV ordinary path succeeds",
                           dirfd, "resolve-xdev", O_RDWR, RESOLVE_NO_XDEV);
    expect_openat2_success("openat2 RESOLVE_NO_MAGICLINKS ordinary path succeeds",
                           dirfd, "resolve-magic", O_RDWR, RESOLVE_NO_MAGICLINKS);
    expect_openat2_success("openat2 RESOLVE_NO_SYMLINKS ordinary path succeeds",
                           dirfd, "resolve-symlink", O_RDWR, RESOLVE_NO_SYMLINKS);
    expect_openat2_success("openat2 RESOLVE_BENEATH ordinary path succeeds",
                           dirfd, "resolve-beneath", O_RDWR, RESOLVE_BENEATH);
    expect_openat2_success("openat2 RESOLVE_IN_ROOT ordinary path succeeds",
                           dirfd, "resolve-in-root", O_RDWR, RESOLVE_IN_ROOT);

    close(dirfd);
    rmdir(dir);
}

static void expect_mmap_einval(const char *name, size_t length, int prot)
{
    errno = 0;
    void *addr = mmap(NULL, length, prot, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (addr != MAP_FAILED) {
        munmap(addr, length);
    }
    CHECK(addr == MAP_FAILED && errno == EINVAL, name);
}

static void test_mmap_min(void)
{
    printf("[TEST] mmap minimal conformance\n");
    expect_mmap_einval("mmap unknown PROT bit -> EINVAL", 4096, PROT_READ | 0x40000000);
    expect_mmap_einval("mmap length zero -> EINVAL", 0, PROT_READ);
}

static char *make_long_path(void)
{
    char *path = malloc(PATH_MAX + 1);
    size_t pos = 0;

    if (!path) {
        return NULL;
    }

    while (pos + 2 < PATH_MAX) {
        path[pos++] = 'a';
        path[pos++] = '/';
    }
    while (pos < PATH_MAX) {
        path[pos++] = 'b';
    }
    path[pos] = '\0';
    return path;
}

static void expect_path_errno(const char *name, int ret, int expected)
{
    if (ret >= 0) {
        close(ret);
    }
    CHECK(ret == -1 && errno == expected, name);
}

static void test_pathmax_min(void)
{
    char *path = make_long_path();
    struct stat st;
#ifdef SYS_statx
    long statx_buf[64];
#endif

    printf("[TEST] PATH_MAX minimal conformance\n");
    CHECK(path != NULL, "allocate PATH_MAX test path");
    if (!path) {
        return;
    }

    errno = 0;
    expect_path_errno("stat long path -> ENAMETOOLONG", stat(path, &st), ENAMETOOLONG);
#ifdef SYS_statx
    errno = 0;
    expect_path_errno("statx long path -> ENAMETOOLONG",
                      syscall(SYS_statx, AT_FDCWD, path, 0, STATX_BASIC_STATS, statx_buf),
                      ENAMETOOLONG);
#endif
    errno = 0;
    expect_path_errno("access long path -> ENAMETOOLONG", access(path, F_OK), ENAMETOOLONG);
    errno = 0;
    expect_path_errno("chdir long path -> ENAMETOOLONG", chdir(path), ENAMETOOLONG);
    errno = 0;
    expect_path_errno("openat long path -> ENAMETOOLONG", openat(AT_FDCWD, path, O_RDONLY),
                      ENAMETOOLONG);

    free(path);
}

int main(void)
{
    printf("================================================\n");
    printf("  TEST: LTP-derived syscall pilot minimal checks\n");
    printf("================================================\n");

    test_openat2_min();
    test_mmap_min();
    test_pathmax_min();

    printf("------------------------------------------------\n");
    printf("  DONE: %d pass, %d fail\n", pass_count, fail_count);
    printf("================================================\n");
    return fail_count == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
