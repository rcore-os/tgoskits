#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int readlink_to(const char *path, char *buf, size_t bufsize)
{
    ssize_t n = readlink(path, buf, bufsize - 1);
    if (n < 0) {
        fprintf(stderr, "FAIL: readlink(%s): %s\n", path, strerror(errno));
        return -1;
    }
    buf[n] = '\0';
    return 0;
}

static int assert_symlink(const char *path)
{
    struct stat st;
    if (lstat(path, &st) != 0) {
        fprintf(stderr, "FAIL: lstat(%s): %s\n", path, strerror(errno));
        return -1;
    }
    if (!S_ISLNK(st.st_mode)) {
        fprintf(stderr, "FAIL: %s is not a symlink (mode=0%o)\n", path, st.st_mode);
        return -1;
    }
    return 0;
}

int main(void)
{
    char buf[PATH_MAX];
    char cwd[PATH_MAX];

    if (assert_symlink("/proc/self/root") < 0) return 1;
    printf("INFO: /proc/self/root is a symlink\n");

    if (readlink_to("/proc/self/root", buf, sizeof(buf)) < 0) return 1;
    if (strcmp(buf, "/") != 0) {
        fprintf(stderr, "FAIL: /proc/self/root -> '%s' (expected '/')\n", buf);
        return 1;
    }
    printf("INFO: /proc/self/root -> /\n");

    if (assert_symlink("/proc/self/cwd") < 0) return 1;
    printf("INFO: /proc/self/cwd is a symlink\n");

    if (getcwd(cwd, sizeof(cwd)) == NULL) {
        perror("FAIL: getcwd");
        return 1;
    }
    if (readlink_to("/proc/self/cwd", buf, sizeof(buf)) < 0) return 1;
    if (strcmp(buf, cwd) != 0) {
        fprintf(stderr, "FAIL: /proc/self/cwd -> '%s' (getcwd='%s')\n", buf, cwd);
        return 1;
    }
    printf("INFO: /proc/self/cwd -> %s (matches getcwd)\n", buf);

    if (chdir("/tmp") != 0) {
        fprintf(stderr, "FAIL: chdir(/tmp): %s\n", strerror(errno));
        return 1;
    }
    if (getcwd(cwd, sizeof(cwd)) == NULL) {
        perror("FAIL: getcwd after chdir");
        return 1;
    }
    if (readlink_to("/proc/self/cwd", buf, sizeof(buf)) < 0) return 1;
    if (strcmp(buf, "/tmp") != 0) {
        fprintf(stderr, "FAIL: after chdir, /proc/self/cwd -> '%s' (expected '/tmp')\n", buf);
        return 1;
    }
    printf("INFO: after chdir, /proc/self/cwd -> /tmp\n");

    if (readlink_to("/proc/self/root", buf, sizeof(buf)) < 0) return 1;
    if (strcmp(buf, "/") != 0) {
        fprintf(stderr, "FAIL: /proc/self/root changed after chdir: '%s'\n", buf);
        return 1;
    }

    if (chdir("/") != 0) {
        fprintf(stderr, "FAIL: chdir(/): %s\n", strerror(errno));
        return 1;
    }
    if (readlink_to("/proc/self/cwd", buf, sizeof(buf)) < 0) return 1;
    if (strcmp(buf, "/") != 0) {
        fprintf(stderr, "FAIL: after chdir(/), /proc/self/cwd -> '%s' (expected '/')\n", buf);
        return 1;
    }
    printf("INFO: after chdir(/), /proc/self/cwd -> /\n");

    printf("TEST_PROC_ROOT_CWD_PASSED\n");
    return 0;
}
