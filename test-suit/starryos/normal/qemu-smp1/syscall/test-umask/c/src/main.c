#define _GNU_SOURCE

#include "test_framework.h"

#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

static char base[PATH_MAX];
static int dirfd = -1;

static void assert_mode_at(int dfd, const char *path, mode_t expected,
                           const char *msg)
{
    struct stat st;
    ASSERT_OK(fstatat(dfd, path, &st, 0), msg);
    mode_t actual = st.st_mode & 07777;
    printf("  MODE | %s | actual=%04o expected=%04o\n",
           msg, actual, expected);
    ASSERT_TRUE(actual == expected, "mode matches expected bits");
}

static void cleanup(void)
{
    if (dirfd >= 0) {
        close(dirfd);
        dirfd = -1;
    }

    char path[PATH_MAX];
    const char *names[] = {
        "file-0027",
        "dir-0027",
        "file-cleared",
        NULL,
    };

    for (int i = 0; names[i] != NULL; i++) {
        int ret = snprintf(path, sizeof(path), "%s/%s", base, names[i]);
        if (ret > 0 && (size_t)ret < sizeof(path)) {
            unlink(path);
            rmdir(path);
        }
    }
    rmdir(base);
}

static void setup(void)
{
    int ret = snprintf(base, sizeof(base), "/tmp/test-umask-%ld",
                       (long)getpid());
    ASSERT_TRUE(ret > 0 && (size_t)ret < sizeof(base), "build test directory");

    cleanup();

    ASSERT_OK(mkdir(base, 0700), "mkdir test directory");
    dirfd = open(base, O_RDONLY | O_DIRECTORY);
    ASSERT_TRUE(dirfd >= 0, "open test directory");
}

static void test_umask_return_and_create_modes(void)
{
    mode_t saved = umask(0);
    printf("  UMASK | saved old mask=%04o\n", saved);

    mode_t previous = umask(0027);
    printf("  UMASK | set 0027 | old=%04o\n", previous);
    ASSERT_TRUE(previous == 0, "umask returns the previous mask");

    int fd = openat(dirfd, "file-0027", O_CREAT | O_RDWR | O_TRUNC, 0666);
    ASSERT_TRUE(fd >= 0, "create regular file affected by umask");
    ASSERT_OK(close(fd), "close regular file");
    assert_mode_at(dirfd, "file-0027", 0640,
                   "regular file mode is masked by umask");

    ASSERT_OK(mkdirat(dirfd, "dir-0027", 0777),
              "mkdir directory affected by umask");
    assert_mode_at(dirfd, "dir-0027", 0750,
                   "directory mode is masked by umask");

    previous = umask(07777);
    printf("  UMASK | set 07777 | old=%04o\n", previous);
    ASSERT_TRUE(previous == 0027, "umask returns old low permission bits");

    previous = umask(0);
    printf("  UMASK | reset 0 | old=%04o\n", previous);
    ASSERT_TRUE(previous == 0777, "umask stores only low 0777 permission bits");

    fd = openat(dirfd, "file-cleared", O_CREAT | O_RDWR | O_TRUNC, 0666);
    ASSERT_TRUE(fd >= 0, "create regular file after clearing umask");
    ASSERT_OK(close(fd), "close cleared-umask file");
    assert_mode_at(dirfd, "file-cleared", 0666,
                   "cleared umask leaves requested file mode");

    umask(saved);
}

int main(void)
{
    TEST_START("umask Linux return semantics");

    setup();
    test_umask_return_and_create_modes();
    cleanup();

    TEST_DONE();
}
