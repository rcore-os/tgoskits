#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_fchmodat2
/* Linux assigns syscall number 452 on x86_64, AArch64, RISC-V, and LoongArch. */
#define SYS_fchmodat2 452
#endif

static int read_mode(int fd, mode_t *mode)
{
    struct stat st;
    if (fstat(fd, &st) < 0)
        return -1;
    *mode = st.st_mode & 07777;
    return 0;
}

static int check_result(const char *name, long rc, int error,
                        int expected_rc, int expected_error, int fd,
                        mode_t expected_mode)
{
    mode_t mode = 0;
    int stat_error = 0;
    if (read_mode(fd, &mode) < 0)
        stat_error = errno;

    int pass = rc == expected_rc &&
               (rc != -1 || error == expected_error) &&
               stat_error == 0 && mode == expected_mode;
    printf("%s: %s rc=%ld errno=%d mode=%04o expected_mode=%04o",
           name, pass ? "PASS" : "FAIL", rc, error, (unsigned)mode,
           (unsigned)expected_mode);
    if (stat_error != 0)
        printf(" fstat_errno=%d (%s)", stat_error, strerror(stat_error));
    putchar('\n');
    return pass;
}

static int restore_mode(int fd, mode_t mode)
{
    errno = 0;
    long rc = syscall(SYS_fchmod, fd, mode);
    int error = errno;
    return check_result("restore", rc, error, 0, 0, fd, mode);
}

int main(void)
{
    char file[] = "/tmp/bug_path_fchmod_bypassXXXXXX";
    char fd_path[32];
    char relative_path[32];
    int result = 0;
    int file_fd = -1;
    int path_fd = -1;
    int proc_fd = -1;
    mode_t original_mode = 0;
    const mode_t requested_mode = 0600;

    file_fd = mkstemp(file);
    if (file_fd < 0) {
        int error = errno;
        printf("FAIL: setup mkstemp errno=%d (%s)\n", error, strerror(error));
        return 1;
    }
    if (syscall(SYS_fchmod, file_fd, 0640) < 0 ||
        read_mode(file_fd, &original_mode) < 0) {
        int error = errno;
        printf("FAIL: setup baseline mode errno=%d (%s)\n", error,
               strerror(error));
        result = 1;
        goto cleanup;
    }
    if (original_mode != 0640) {
        printf("FAIL: setup baseline mode=%04o expected=0640\n",
               (unsigned)original_mode);
        result = 1;
        goto cleanup;
    }

    path_fd = open(file, O_PATH);
    if (path_fd < 0) {
        int error = errno;
        printf("FAIL: setup open(O_PATH) errno=%d (%s)\n", error,
               strerror(error));
        result = 1;
        goto cleanup;
    }
    proc_fd = open("/proc/self/fd", O_RDONLY | O_DIRECTORY);
    if (proc_fd < 0) {
        int error = errno;
        printf("FAIL: setup open(/proc/self/fd) errno=%d (%s)\n", error,
               strerror(error));
        result = 1;
        goto cleanup;
    }
    if (snprintf(fd_path, sizeof(fd_path), "/proc/self/fd/%d", path_fd) < 0 ||
        snprintf(relative_path, sizeof(relative_path), "%d", path_fd) < 0) {
        printf("FAIL: setup format procfd path\n");
        result = 1;
        goto cleanup;
    }

    errno = 0;
    long rc = syscall(SYS_fchmod, path_fd, requested_mode);
    int error = errno;
    result |= !check_result("fchmod(O_PATH)", rc, error, -1, EBADF,
                            file_fd, original_mode);

    result |= !restore_mode(file_fd, original_mode);
    errno = 0;
    rc = syscall(SYS_fchmodat2, path_fd, "", requested_mode, AT_EMPTY_PATH);
    error = errno;
    result |= !check_result("fchmodat2(AT_EMPTY_PATH)", rc, error, 0, 0,
                            file_fd, requested_mode);

    result |= !restore_mode(file_fd, original_mode);
    errno = 0;
    rc = syscall(SYS_fchmodat, AT_FDCWD, fd_path, requested_mode);
    error = errno;
    result |= !check_result("fchmodat(procfd absolute)", rc, error, 0, 0,
                            file_fd, requested_mode);

    result |= !restore_mode(file_fd, original_mode);
    errno = 0;
    rc = syscall(SYS_fchmodat, proc_fd, relative_path, requested_mode);
    error = errno;
    result |= !check_result("fchmodat(procfd relative)", rc, error, 0, 0,
                            file_fd, requested_mode);

cleanup:
    if (proc_fd >= 0)
        close(proc_fd);
    if (path_fd >= 0)
        close(path_fd);
    if (file_fd >= 0)
        close(file_fd);
    unlink(file);
    return result ? 1 : 0;
}
