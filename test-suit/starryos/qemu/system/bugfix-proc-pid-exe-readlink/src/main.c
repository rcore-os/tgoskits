#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#define BIND_EXECUTABLE_PATH "/tmp/starry-proc-pid-exe-readlink-bind"

static int failures;

static void check(int condition, const char *message)
{
    if (condition) {
        printf("PASS: %s\n", message);
        return;
    }

    fprintf(stderr, "FAIL: %s errno=%d (%s)\n", message, errno, strerror(errno));
    failures++;
}

static void check_proc_exe(pid_t pid, const char *description,
                           const char *expected_target, int pinned)
{
    char proc_path[64];
    char target[PATH_MAX];
    char canonical[PATH_MAX];

    snprintf(proc_path, sizeof(proc_path), "/proc/%ld/exe", (long)pid);
    errno = 0;
    ssize_t target_len =
        syscall(SYS_readlinkat, AT_FDCWD, proc_path, target, sizeof(target) - 1);
    check(target_len > 0, description);
    if (target_len <= 0) {
        return;
    }

    target[target_len] = '\0';
    printf("INFO: %s target=%s\n", proc_path, target);
    check(target[0] == '/', "proc executable target is absolute");
    if (expected_target != NULL) {
        check(strcmp(target, expected_target) == 0,
              "proc executable target matches the expected path");
    }

    errno = 0;
    if (pinned < 0) {
        char *resolved = realpath(target, canonical);
        check(resolved != NULL,
              "linked proc executable target is canonicalizable like readlink -f");
        if (resolved == NULL) return;
    }

    errno = 0;
    int fd = pinned >= 0 ? dup(pinned) : open(canonical, O_RDONLY | O_CLOEXEC);
    check(fd >= 0, "executable identity reference is openable");
    if (fd < 0) {
        return;
    }

    struct stat statbuf = {0}, followed = {0}, opened = {0}, link = {0};
    check(fstat(fd, &statbuf) == 0, "opened executable target is statable");
    check(S_ISREG(statbuf.st_mode), "opened executable target is a regular file");
    int procfd = syscall(SYS_openat, AT_FDCWD, proc_path, O_RDONLY | O_CLOEXEC, 0);
    check(procfd >= 0 && fstat(procfd, &opened) == 0 &&
          opened.st_dev == statbuf.st_dev && opened.st_ino == statbuf.st_ino,
          "proc exe opens the actual executed object");
    check(fstatat(AT_FDCWD, proc_path, &followed, 0) == 0 &&
          S_ISREG(followed.st_mode) &&
          followed.st_dev == statbuf.st_dev && followed.st_ino == statbuf.st_ino,
          "stat proc exe reports executable identity, not link metadata");
    check(fstatat(AT_FDCWD, proc_path, &link, AT_SYMLINK_NOFOLLOW) == 0 &&
          S_ISLNK(link.st_mode), "lstat proc exe reports a symlink");
    int linkfd = syscall(SYS_openat, AT_FDCWD, proc_path,
                         O_PATH | O_NOFOLLOW | O_CLOEXEC, 0);
    check(linkfd >= 0 && fstat(linkfd, &opened) == 0 &&
          S_ISLNK(opened.st_mode) && opened.st_dev == link.st_dev &&
          opened.st_ino == link.st_ino, "O_PATH nofollow pins the exe link");
    if (linkfd >= 0) close(linkfd);
    if (procfd >= 0) close(procfd);
    close(fd);
}

static int hold_exec_child(int ready_fd, int release_fd)
{
    char byte = 'R';
    if (write(ready_fd, &byte, 1) != 1) {
        return 2;
    }
    if (read(release_fd, &byte, 1) != 1) {
        return 3;
    }
    return 0;
}

static void check_exec_child(const char *executable_path,
                             const char *expected_proc_exe, int remove_name)
{
    int ready_pipe[2] = {-1, -1};
    int release_pipe[2] = {-1, -1};
    int pinned = remove_name ? open(executable_path, O_RDONLY | O_CLOEXEC) : -1;
    if (remove_name && pinned < 0) {
        check(0, "pin executable identity before exec");
        return;
    }
    check(pipe(ready_pipe) == 0, "create exec child ready pipe");
    check(pipe(release_pipe) == 0, "create exec child release pipe");
    if (failures != 0) {
        for (int i = 0; i < 2; ++i) {
            if (ready_pipe[i] >= 0) close(ready_pipe[i]);
            if (release_pipe[i] >= 0) close(release_pipe[i]);
        }
        if (pinned >= 0) close(pinned);
        return;
    }

    pid_t child = fork();
    check(child >= 0, "fork exec child");
    if (child < 0) {
        close(ready_pipe[0]);
        close(ready_pipe[1]);
        close(release_pipe[0]);
        close(release_pipe[1]);
        if (pinned >= 0) close(pinned);
        return;
    }

    if (child == 0) {
        char ready_fd[16];
        char release_fd[16];

        close(ready_pipe[0]);
        close(release_pipe[1]);
        snprintf(ready_fd, sizeof(ready_fd), "%d", ready_pipe[1]);
        snprintf(release_fd, sizeof(release_fd), "%d", release_pipe[0]);
        execl(executable_path, "bugfix-proc-pid-exe-readlink", "--hold-exec-child",
              ready_fd, release_fd, NULL);
        _exit(127);
    }

    close(ready_pipe[1]);
    close(release_pipe[0]);

    char ready = '\0';
    check(read(ready_pipe[0], &ready, 1) == 1 && ready == 'R',
          "exec child reached held state");
    if (ready == 'R') {
        char deleted[PATH_MAX];
        if (remove_name) {
            check(unlink(executable_path) == 0, "unlink the held child's executable");
            snprintf(deleted, sizeof(deleted), "%s (deleted)", executable_path);
            expected_proc_exe = deleted;
        }
        check_proc_exe(child, "readlinkat returns exec child executable target",
                       expected_proc_exe, pinned);
    }

    char release = 'X';
    if (ready == 'R')
        check(write(release_pipe[1], &release, 1) == 1, "release exec child");
    close(release_pipe[1]);

    int status = 0;
    check(waitpid(child, &status, 0) == child, "reap exec child");
    check(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "exec child exits after procfs inspection");

    close(ready_pipe[0]);
    if (pinned >= 0) close(pinned);
}

static void check_bind_mounted_exec(const char *self_path)
{
    unlink(BIND_EXECUTABLE_PATH);

    int target_fd = open(BIND_EXECUTABLE_PATH,
                         O_WRONLY | O_CREAT | O_CLOEXEC | O_TRUNC, 0700);
    check(target_fd >= 0, "create bind-mounted executable target");
    if (target_fd < 0) {
        return;
    }
    close(target_fd);

    int mounted = mount(self_path, BIND_EXECUTABLE_PATH, NULL, MS_BIND, NULL) == 0;
    check(mounted, "bind mount executable at a distinct absolute path");
    if (mounted) {
        check_exec_child(BIND_EXECUTABLE_PATH, BIND_EXECUTABLE_PATH, 0);
        check(umount(BIND_EXECUTABLE_PATH) == 0,
              "unmount bind-mounted executable target");
    }

    check(unlink(BIND_EXECUTABLE_PATH) == 0,
          "remove bind-mounted executable target");
}

/* Reuse the held self-exec flow, but remove the executable's only name.
 * An implementation that resolves readlink text cannot reopen this object. */
static void check_unlinked_exec(const char *self_path)
{
    char copy[] = "/tmp/starry-proc-exe-XXXXXX";
    int output = mkstemp(copy);
    int input = open(self_path, O_RDONLY | O_CLOEXEC);
    int copied = output >= 0 && input >= 0;
    char buffer[16384];
    while (copied) {
        ssize_t n = read(input, buffer, sizeof(buffer));
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) {
            if (n < 0) copied = 0;
            break;
        }
        ssize_t offset = 0;
        while (offset < n) {
            ssize_t count = write(output, buffer + offset, (size_t)(n - offset));
            if (count < 0 && errno == EINTR) continue;
            if (count <= 0) {
                copied = 0;
                break;
            }
            offset += count;
        }
    }
    if (copied && fchmod(output, 0700) != 0) copied = 0;
    if (input >= 0) close(input);
    if (output >= 0 && close(output) != 0) copied = 0;
    check(copied, "SETUP private executable copy");
    if (copied) check_exec_child(copy, copy, 1);
    check(unlink(copy) == 0 || errno == ENOENT, "clean private executable copy");
}

int main(int argc, char **argv)
{
    if (argc == 4 && strcmp(argv[1], "--hold-exec-child") == 0) {
        return hold_exec_child(atoi(argv[2]), atoi(argv[3]));
    }

    check_proc_exe(1, "readlinkat returns PID 1 executable target", NULL, -1);
    check_proc_exe(getpid(), "readlinkat returns current executable target", NULL, -1);
    check(argv[0][0] == '/', "grouped runner invokes the test by absolute path");
    if (argv[0][0] == '/') {
        check_exec_child(argv[0], argv[0], 0);
        check_bind_mounted_exec(argv[0]);
        check_unlinked_exec(argv[0]);
    }

    if (failures != 0) {
        fprintf(stderr, "STARRY_PROC_PID_EXE_READLINK_FAILED: %d checks\n", failures);
        return 1;
    }

    puts("STARRY_PROC_PID_EXE_READLINK_PASSED");
    return 0;
}
