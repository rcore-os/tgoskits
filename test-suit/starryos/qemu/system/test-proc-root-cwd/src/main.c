#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
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

static int check_getcwd_relative_to_process_root(void)
{
    char jail[] = "/tmp/proc-root-cwd-XXXXXX";
    char child_dir[PATH_MAX];
    int outside_fd = open("/", O_RDONLY | O_DIRECTORY);
    if (outside_fd < 0 || mkdtemp(jail) == NULL) {
        perror("FAIL: prepare getcwd chroot test");
        if (outside_fd >= 0) close(outside_fd);
        return -1;
    }
    int path_len = snprintf(child_dir, sizeof(child_dir), "%s/child", jail);
    if (path_len < 0 || (size_t)path_len >= sizeof(child_dir) ||
        mkdir(child_dir, 0700) < 0) {
        perror("FAIL: create getcwd chroot child");
        rmdir(jail);
        close(outside_fd);
        return -1;
    }

    pid_t child = fork();
    if (child == 0) {
        char cwd[PATH_MAX];
        cwd[0] = '\0';
        if (chroot(jail) < 0 || chdir("/child") < 0) {
            perror("FAIL: enter getcwd chroot");
            _exit(1);
        }
        long size = syscall(SYS_getcwd, cwd, sizeof(cwd));
        if (size != (long)sizeof("/child") || strcmp(cwd, "/child") != 0) {
            fprintf(stderr, "FAIL: chroot getcwd -> '%s', length %ld\n", cwd, size);
            _exit(1);
        }
        if (fchdir(outside_fd) < 0) {
            perror("FAIL: leave getcwd chroot through existing fd");
            _exit(1);
        }
        size = syscall(SYS_getcwd, cwd, sizeof(cwd));
        if (size != (long)sizeof("(unreachable)/") ||
            strcmp(cwd, "(unreachable)/") != 0) {
            fprintf(stderr, "FAIL: unreachable getcwd -> '%s', length %ld\n", cwd, size);
            _exit(1);
        }
        _exit(0);
    }

    int status = 0;
    int waited = child < 0 ? -1 : waitpid(child, &status, 0);
    int cleanup_child = rmdir(child_dir);
    int cleanup_jail = rmdir(jail);
    close(outside_fd);
    if (child < 0 || waited != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0 ||
        cleanup_child < 0 || cleanup_jail < 0) {
        fprintf(stderr, "FAIL: getcwd chroot child or cleanup failed\n");
        return -1;
    }
    printf("INFO: raw getcwd follows process root and marks unreachable cwd\n");
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

    if (check_getcwd_relative_to_process_root() < 0) return 1;

    printf("TEST_PROC_ROOT_CWD_PASSED\n");
    return 0;
}
