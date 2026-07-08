#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

static int passed;
static int failed;

#define CHECK(cond, msg)                                                        \
    do {                                                                        \
        if (cond) {                                                             \
            printf("  PASS | %s\n", (msg));                                    \
            passed++;                                                           \
        } else {                                                                \
            printf("  FAIL | %s | errno=%d (%s)\n", (msg), errno,              \
                   strerror(errno));                                            \
            failed++;                                                           \
        }                                                                       \
    } while (0)

static long do_mknodat(int dirfd, const char *path, mode_t mode, dev_t dev)
{
    return syscall(SYS_mknodat, dirfd, path, mode, dev);
}

static void cleanup_child(int dirfd, const char *name)
{
    unlinkat(dirfd, name, 0);
    unlinkat(dirfd, name, AT_REMOVEDIR);
}

static void expect_regular(int dirfd, const char *name, mode_t mode,
                           const char *label)
{
    struct stat st;

    errno = 0;
    CHECK(fstatat(dirfd, name, &st, AT_SYMLINK_NOFOLLOW) == 0, label);
    if (errno == 0) {
        CHECK(S_ISREG(st.st_mode), "created node is a regular file");
        CHECK((st.st_mode & 0777) == mode, "created regular file mode matches");
    }
}

int main(void)
{
    char base[128];
    int dirfd;
    long rc;
    mode_t old_umask;

    old_umask = umask(0);
    snprintf(base, sizeof(base), "/tmp/bug_mknodat_mode_type_%ld", (long)getpid());
    unlink(base);
    rmdir(base);

    printf("TEST: bug-mknodat-mode-type\n");

    errno = 0;
    CHECK(mkdir(base, 0700) == 0, "create private test directory");

    dirfd = open(base, O_RDONLY | O_DIRECTORY);
    CHECK(dirfd >= 0, "open private test directory");
    if (dirfd < 0) {
        umask(old_umask);
        return 1;
    }

    errno = 0;
    rc = do_mknodat(dirfd, "zero-type", 0600, 0);
    CHECK(rc == 0, "mknodat mode without S_IFMT creates a file");
    expect_regular(dirfd, "zero-type", 0600, "stat zero-type result");

    errno = 0;
    rc = do_mknodat(dirfd, "zero-type", 0600, 0);
    CHECK(rc == -1 && errno == EEXIST,
          "mknodat zero-type existing path returns EEXIST");
    expect_regular(dirfd, "zero-type", 0600,
                   "existing path remains the original regular file");

    errno = 0;
    rc = do_mknodat(dirfd, "explicit-reg", S_IFREG | 0640, 0);
    CHECK(rc == 0, "mknodat explicit S_IFREG still creates a file");
    expect_regular(dirfd, "explicit-reg", 0640, "stat explicit S_IFREG result");

    errno = 0;
    rc = do_mknodat(dirfd, "dir-type", S_IFDIR | 0700, 0);
    CHECK(rc == -1 && errno == EPERM, "mknodat S_IFDIR returns EPERM");
    errno = 0;
    CHECK(faccessat(dirfd, "dir-type", F_OK, 0) == -1 && errno == ENOENT,
          "mknodat S_IFDIR leaves no directory entry");

    errno = 0;
    rc = do_mknodat(dirfd, "symlink-type", S_IFLNK | 0777, 0);
    CHECK(rc == -1 && errno == EINVAL, "mknodat S_IFLNK returns EINVAL");
    errno = 0;
    CHECK(faccessat(dirfd, "symlink-type", F_OK, 0) == -1 && errno == ENOENT,
          "mknodat S_IFLNK leaves no directory entry");

    cleanup_child(dirfd, "zero-type");
    cleanup_child(dirfd, "explicit-reg");
    close(dirfd);
    rmdir(base);
    umask(old_umask);

    printf("RESULT: %d passed / %d failed\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
    }
    return failed == 0 ? 0 : 1;
}
