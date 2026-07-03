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
#include <sys/statfs.h>
#include <sys/inotify.h>
#include <sys/xattr.h>
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

#ifndef RESOLVE_CACHED
#define RESOLVE_CACHED         0x20
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
    expect_openat2_errno("openat2 unsupported RESOLVE_NO_XDEV -> EOPNOTSUPP",
                         dirfd, "resolve-xdev",
                         (struct open_how){ O_RDWR | O_CREAT, 0600, RESOLVE_NO_XDEV },
                         sizeof(struct open_how), EOPNOTSUPP);
    expect_openat2_errno("openat2 unsupported RESOLVE_NO_MAGICLINKS -> EOPNOTSUPP",
                         dirfd, "resolve-magic",
                         (struct open_how){ O_RDWR | O_CREAT, 0600, RESOLVE_NO_MAGICLINKS },
                         sizeof(struct open_how), EOPNOTSUPP);
    expect_openat2_errno("openat2 unsupported RESOLVE_NO_SYMLINKS -> EOPNOTSUPP",
                         dirfd, "resolve-symlink",
                         (struct open_how){ O_RDWR | O_CREAT, 0600, RESOLVE_NO_SYMLINKS },
                         sizeof(struct open_how), EOPNOTSUPP);
    expect_openat2_errno("openat2 unsupported RESOLVE_BENEATH -> EOPNOTSUPP",
                         dirfd, "resolve-beneath",
                         (struct open_how){ O_RDWR | O_CREAT, 0600, RESOLVE_BENEATH },
                         sizeof(struct open_how), EOPNOTSUPP);
    expect_openat2_errno("openat2 unsupported RESOLVE_IN_ROOT -> EOPNOTSUPP",
                         dirfd, "resolve-in-root",
                         (struct open_how){ O_RDWR | O_CREAT, 0600, RESOLVE_IN_ROOT },
                         sizeof(struct open_how), EOPNOTSUPP);
    expect_openat2_errno("openat2 unsupported RESOLVE_CACHED -> EOPNOTSUPP",
                         dirfd, "resolve-cached",
                         (struct open_how){ O_RDWR | O_CREAT, 0600, RESOLVE_CACHED },
                         sizeof(struct open_how), EOPNOTSUPP);

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

struct mmap_maps_case {
    int prot;
    int flags;
    const char *expected;
};

static int read_maps_perms(void *addr, char perms[5])
{
    FILE *maps = fopen("/proc/self/maps", "r");
    unsigned long target = (unsigned long)addr;
    char line[256];

    if (!maps) {
        return -1;
    }

    while (fgets(line, sizeof(line), maps)) {
        unsigned long start = 0;
        unsigned long end = 0;
        char found[5] = {0};

        if (sscanf(line, "%lx-%lx %4s", &start, &end, found) == 3 &&
            start == target && target < end) {
            memcpy(perms, found, sizeof(found));
            fclose(maps);
            return 0;
        }
    }

    fclose(maps);
    errno = ENOENT;
    return -1;
}

static void expect_mmap_maps_perms(const struct mmap_maps_case *tc)
{
    long pagesize = sysconf(_SC_PAGESIZE);
    int guard_flags = (tc->flags & MAP_PRIVATE) ? MAP_SHARED : MAP_PRIVATE;
    void *addr1;
    void *addr2;
    char perms[5] = {0};
    char name[96];

    if (pagesize <= 0) {
        CHECK(0, "mmap maps perms setup pagesize");
        return;
    }

    addr1 = mmap(NULL, (size_t)pagesize * 2, PROT_NONE,
                 MAP_ANONYMOUS | guard_flags, -1, 0);
    if (addr1 == MAP_FAILED) {
        CHECK(0, "mmap maps perms setup guard mapping");
        return;
    }

    addr2 = mmap((char *)addr1 + pagesize, (size_t)pagesize, tc->prot,
                 tc->flags | MAP_FIXED, -1, 0);
    if (addr2 == MAP_FAILED) {
        munmap(addr1, (size_t)pagesize * 2);
        CHECK(0, "mmap maps perms setup fixed mapping");
        return;
    }

    snprintf(name, sizeof(name), "mmap /proc/self/maps perms %s", tc->expected);
    if (read_maps_perms(addr2, perms) != 0) {
        CHECK(0, name);
    } else {
        CHECK(strcmp(perms, tc->expected) == 0, name);
        if (strcmp(perms, tc->expected) != 0) {
            printf("    expected=%s found=%s\n", tc->expected, perms);
        }
    }

    munmap(addr1, (size_t)pagesize * 2);
}

static void test_mmap_maps_min(void)
{
    static const struct mmap_maps_case tcases[] = {
        {PROT_NONE, MAP_ANONYMOUS | MAP_PRIVATE, "---p"},
        {PROT_NONE, MAP_ANONYMOUS | MAP_SHARED, "---s"},
        {PROT_READ, MAP_ANONYMOUS | MAP_PRIVATE, "r--p"},
        {PROT_READ, MAP_ANONYMOUS | MAP_SHARED, "r--s"},
        {PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE, "-w-p"},
        {PROT_WRITE, MAP_ANONYMOUS | MAP_SHARED, "-w-s"},
        {PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE, "rw-p"},
        {PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_SHARED, "rw-s"},
        {PROT_READ | PROT_EXEC, MAP_ANONYMOUS | MAP_PRIVATE, "r-xp"},
        {PROT_READ | PROT_EXEC, MAP_ANONYMOUS | MAP_SHARED, "r-xs"},
        {PROT_WRITE | PROT_EXEC, MAP_ANONYMOUS | MAP_PRIVATE, "-wxp"},
        {PROT_WRITE | PROT_EXEC, MAP_ANONYMOUS | MAP_SHARED, "-wxs"},
        {PROT_READ | PROT_WRITE | PROT_EXEC, MAP_ANONYMOUS | MAP_PRIVATE, "rwxp"},
        {PROT_READ | PROT_WRITE | PROT_EXEC, MAP_ANONYMOUS | MAP_SHARED, "rwxs"},
    };
    size_t i;

    printf("[TEST] mmap /proc/self/maps permissions\n");
    for (i = 0; i < sizeof(tcases) / sizeof(tcases[0]); i++) {
        expect_mmap_maps_perms(&tcases[i]);
    }
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

static void test_pipe_efault_min(void)
{
    int marker;
    int fds[2] = {-1, -1};
    long ret;

    printf("[TEST] pipe bad user pointer rollback\n");

    marker = open("/dev/null", O_RDONLY);
    if (marker < 0) {
        marker = open(".", O_RDONLY | O_DIRECTORY);
    }
    CHECK(marker >= 0, "pipe rollback setup marker fd");
    if (marker < 0) {
        return;
    }

#ifdef SYS_pipe2
    errno = 0;
    ret = syscall(SYS_pipe2, (int *)-1, 0);
    CHECK(ret == -1 && errno == EFAULT, "pipe2 bad user fd array -> EFAULT");
#else
    errno = ENOSYS;
    ret = -1;
    CHECK(0, "pipe2 syscall number available");
#endif

    if (ret == -1 && errno == EFAULT) {
        CHECK(pipe(fds) == 0, "pipe succeeds after pipe2 EFAULT");
        if (fds[0] >= 0 && fds[1] >= 0) {
            CHECK(fds[0] == marker + 1 && fds[1] == marker + 2,
                  "pipe2 EFAULT does not leak allocated fds");
        }
    }

    if (fds[0] >= 0)
        close(fds[0]);
    if (fds[1] >= 0)
        close(fds[1]);
    close(marker);
}

static void expect_path_errno(const char *name, int ret, int expected)
{
    if (ret >= 0) {
        close(ret);
    }
    CHECK(ret == -1 && errno == expected, name);
}

static void expect_ret_errno(const char *name, long ret, int expected)
{
    CHECK(ret == -1 && errno == expected, name);
}

struct test_file_handle {
    unsigned int handle_bytes;
    int handle_type;
    unsigned char f_handle[8];
};

static void test_pathmax_min(void)
{
    char *path = make_long_path();
    struct stat st;
    struct statfs sfs;
    int ifd;
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
    errno = 0;
    expect_path_errno("chroot long path -> ENAMETOOLONG", chroot(path), ENAMETOOLONG);

    ifd = inotify_init1(IN_CLOEXEC);
    CHECK(ifd >= 0, "inotify setup fd");
    if (ifd >= 0) {
        errno = 0;
        CHECK(inotify_add_watch(ifd, path, IN_MODIFY) == -1 && errno == ENAMETOOLONG,
              "inotify_add_watch long path -> ENAMETOOLONG");
        close(ifd);
    }

    errno = 0;
    expect_ret_errno("mknod long path -> ENAMETOOLONG",
                     mknod(path, S_IFIFO | 0600, 0), ENAMETOOLONG);

    errno = 0;
    expect_ret_errno("statfs long path -> ENAMETOOLONG", statfs(path, &sfs),
                     ENAMETOOLONG);

#ifdef SYS_name_to_handle_at
    {
        struct test_file_handle handle = {
            .handle_bytes = sizeof(handle.f_handle),
        };
        int mount_id = -1;

        errno = 0;
        expect_ret_errno("name_to_handle_at long path -> ENAMETOOLONG",
                         syscall(SYS_name_to_handle_at, AT_FDCWD, path, &handle,
                                 &mount_id, 0),
                         ENAMETOOLONG);
    }
#endif

    errno = 0;
    expect_ret_errno("getxattr long path -> ENAMETOOLONG",
                     getxattr(path, "user.test", NULL, 0), ENAMETOOLONG);

    free(path);
}

int main(void)
{
    printf("================================================\n");
    printf("  TEST: LTP-derived syscall pilot minimal checks\n");
    printf("================================================\n");

    test_openat2_min();
    test_mmap_min();
    test_mmap_maps_min();
    test_pathmax_min();
    test_pipe_efault_min();

    printf("------------------------------------------------\n");
    printf("  DONE: %d pass, %d fail\n", pass_count, fail_count);
    printf("================================================\n");
    return fail_count == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
