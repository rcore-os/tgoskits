#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pwd.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <sys/statvfs.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static int expect_mode(const char *path, int mode, int flags, int expected)
{
    errno = 0;
    long result = syscall(SYS_faccessat2, AT_FDCWD, path, mode, flags);
    if ((!expected && result == 0) || (expected && result == -1 && errno == expected))
        return 0;
    fprintf(stderr, "access context: %s flags=%d result=%ld errno=%d expected=%d\n",
            path, flags, result, errno, expected);
    return 1;
}

static int expect_access(const char *path, int flags, int expected)
{
    return expect_mode(path, W_OK, flags, expected);
}

static int check_search_paths(uid_t uid)
{
    char directory[] = "/tmp/ltp-access-search.XXXXXX";
    int result = 1, created = 0, cwd = open(".", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    if (cwd < 0 || !mkdtemp(directory))
        goto cleanup;
    created = 1;
    if (chmod(directory, 0755) || chdir(directory) || mkdir("locked", 0755))
        goto cleanup;
    int fd = open("plain", O_CREAT | O_WRONLY | O_CLOEXEC, 0644);
    if (fd < 0)
        goto cleanup;
    close(fd);
    if (symlink("locked", "link") || symlink("locked/.", "dotlink") || chmod("locked", 0))
        goto cleanup;
    pid_t child = fork();
    if (child < 0)
        goto cleanup;
    if (!child) {
        if (seteuid(uid))
            _exit(2);
        static const struct {
            const char *path;
            int flags;
            int error;
        } cases[] = {
            {"locked", 0, 0},
            {"locked/", 0, 0},
            {"locked/.", 0, EACCES},
            {"locked/./", 0, EACCES},
            {"locked/..", 0, EACCES},
            {"locked/missing", 0, EACCES},
            {"link", AT_SYMLINK_NOFOLLOW, 0},
            {"link/", AT_SYMLINK_NOFOLLOW, 0},
            {"link/.", AT_SYMLINK_NOFOLLOW, EACCES},
            {"dotlink", 0, EACCES},
            {"dotlink", AT_SYMLINK_NOFOLLOW, 0},
            {"plain/", 0, ENOTDIR},
            {"plain/.", AT_SYMLINK_NOFOLLOW, ENOTDIR},
        };
        int failed = 0;
        for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); i++)
            failed |= expect_mode(cases[i].path, F_OK, cases[i].flags | AT_EACCESS, cases[i].error);
        if (seteuid(0) || chdir("locked") || setuid(uid))
            _exit(3);
        failed |= expect_mode(".", F_OK, 0, EACCES);
        failed |= expect_mode("./", F_OK, 0, EACCES);
        failed |= expect_mode("", F_OK, AT_EMPTY_PATH, 0);
        struct stat metadata;
        if (fstatat(AT_FDCWD, "", &metadata, AT_EMPTY_PATH)) {
            perror("fstatat empty cwd");
            failed = 1;
        }
        errno = 0;
        if (fstat(AT_FDCWD, &metadata) != -1 || errno != EBADF) {
            fprintf(stderr, "fstat must reject AT_FDCWD as an ordinary fd\n");
            failed = 1;
        }
        errno = 0;
        if (syscall(SYS_fchmod, AT_FDCWD, 0) != -1 || errno != EBADF) {
            fprintf(stderr, "fchmod must reject AT_FDCWD: errno=%d\n", errno);
            failed = 1;
        }
        errno = 0;
        if (syscall(SYS_fchown, AT_FDCWD, -1, -1) != -1 || errno != EBADF) {
            fprintf(stderr, "fchown must reject AT_FDCWD: errno=%d\n", errno);
            failed = 1;
        }
        errno = 0;
        if (syscall(SYS_utimensat, AT_FDCWD, NULL, NULL, 0) != -1 || errno != EFAULT) {
            fprintf(stderr, "utimensat cwd NULL without AT_EMPTY_PATH: errno=%d\n", errno);
            failed = 1;
        }
        _exit(failed);
    }
    int status;
    if (waitpid(child, &status, 0) == child && WIFEXITED(status) && !WEXITSTATUS(status)) {
        puts("ACCESS_DIRECTORY_SEARCH_PATHS_PASSED");
        result = 0;
    }
cleanup:
    if (created) {
        if (!chdir(directory)) {
            unlink("dotlink");
            unlink("link");
            unlink("plain");
            chmod("locked", 0755);
            rmdir("locked");
        }
    }
    if (cwd >= 0) {
        if (fchdir(cwd))
            result = 1;
        close(cwd);
    }
    if (created)
        rmdir(directory);
    return result;
}

static int check_credentials(uid_t uid)
{
    char directory[] = "/tmp/ltp-access-cred.XXXXXX";
    char path[128] = {0};
    int result = 1, created = 0;
    if (!mkdtemp(directory))
        goto cleanup;
    created = 1;
    if (chmod(directory, 0755))
        goto cleanup;
    snprintf(path, sizeof(path), "%s/owner", directory);
    int fd = open(path, O_CREAT | O_WRONLY | O_CLOEXEC, 0600);
    if (fd < 0)
        goto cleanup;
    close(fd);
    pid_t child = fork();
    if (child < 0)
        goto cleanup;
    if (!child) {
        if (seteuid(uid))
            _exit(2);
        int failed = expect_access(path, 0, 0);
        failed |= expect_access(path, AT_EACCESS, EACCES);
        if (seteuid(0) || setreuid(uid, 0))
            _exit(3);
        failed |= expect_access(path, 0, EACCES);
        failed |= expect_access(path, AT_EACCESS, 0);
        _exit(failed);
    }
    int status;
    if (waitpid(child, &status, 0) == child && WIFEXITED(status) && !WEXITSTATUS(status)) {
        puts("ACCESS_REAL_AND_EFFECTIVE_IDENTITIES_PASSED");
        result = 0;
    }
cleanup:
    if (result)
        perror("credential access context");
    if (created) {
        if (path[0])
            unlink(path);
        rmdir(directory);
    }
    return result;
}

static int option_mode(const char *options, int readonly)
{
    return !strncmp(options, readonly ? "ro" : "rw", 2) &&
           (options[2] == ',' || options[2] == '\0');
}

static int check_mount_reports(const char *path, int mount_ro, int filesystem_ro)
{
    struct statfs status;
    if (statfs(path, &status) || !!(status.f_flags & ST_RDONLY) != (mount_ro || filesystem_ro)) {
        fprintf(stderr, "statfs readonly scope mismatch: %s\n", path);
        return 1;
    }
    FILE *stream = fopen("/proc/self/mountinfo", "r");
    if (!stream)
        return 1;
    char line[4096], point[256], options[256], super_options[256];
    int found = 0;
    while (fgets(line, sizeof(line), stream)) {
        if (sscanf(line, "%*u %*u %*s %*s %255s %255s", point, options) != 2 || strcmp(point, path))
            continue;
        char *separator = strstr(line, " - ");
        found = separator && sscanf(separator + 3, "%*s %*s %255s", super_options) == 1 &&
                option_mode(options, mount_ro) && option_mode(super_options, filesystem_ro);
        break;
    }
    fclose(stream);
    if (!found) {
        fprintf(stderr, "mountinfo conflated mount and filesystem flags: %s\n", path);
        return 1;
    }
    stream = fopen("/proc/mounts", "r");
    if (!stream)
        return 1;
    found = 0;
    while (fgets(line, sizeof(line), stream)) {
        if (sscanf(line, "%*s %255s %*s %255s", point, options) == 2 && !strcmp(point, path)) {
            found = option_mode(options, mount_ro || filesystem_ro);
            break;
        }
    }
    fclose(stream);
    if (!found)
        fprintf(stderr, "mounts effective readonly mismatch: %s\n", path);
    return !found;
}

static int check_readonly_scopes(uid_t uid)
{
    char directory[] = "/tmp/ltp-access-mount.XXXXXX";
    char source[128] = {0}, alias[128] = {0}, source_file[160], alias_file[160];
    int command[2] = {-1, -1}, reply[2] = {-1, -1};
    int mounted = 0, bound = 0, result = 1, created = 0;
    pid_t child = -1;
    if (!mkdtemp(directory))
        goto cleanup;
    created = 1;
    if (chmod(directory, 0755))
        goto cleanup;
    snprintf(source, sizeof(source), "%s/source", directory);
    snprintf(alias, sizeof(alias), "%s/alias", directory);
    snprintf(source_file, sizeof(source_file), "%s/data", source);
    snprintf(alias_file, sizeof(alias_file), "%s/data", alias);
    if (mkdir(source, 0755) || mkdir(alias, 0755) || mount("none", source, "tmpfs", 0, NULL))
        goto cleanup;
    mounted = 1;
    if (chmod(source, 0755))
        goto cleanup;
    int fd = open(source_file, O_CREAT | O_RDWR | O_CLOEXEC, 0666);
    if (fd < 0)
        goto cleanup;
    int changed = fchmod(fd, 0777);
    close(fd);
    if (changed || mount(source, alias, NULL, MS_BIND, NULL))
        goto cleanup;
    bound = 1;
    if (mount(NULL, alias, NULL, MS_REMOUNT | MS_BIND | MS_RDONLY | MS_NOEXEC, NULL) ||
        pipe(command) || pipe(reply))
        goto cleanup;
    child = fork();
    if (child < 0)
        goto cleanup;
    if (!child) {
        close(command[1]);
        close(reply[0]);
        if (unshare(CLONE_NEWNS) || setuid(uid))
            _exit(2);
        char token = 'x';
        if (write(reply[1], &token, 1) != 1)
            _exit(3);
        int failed = 0;
        for (int phase = 0; phase < 3; phase++) {
            if (read(command[0], &token, 1) != 1)
                _exit(4);
            int fs_ro = phase == 1;
            failed |= expect_access(source, 0, fs_ro ? EROFS : EACCES);
            failed |= expect_access(alias, 0, fs_ro ? EROFS : EACCES);
            failed |= expect_access(source_file, 0, fs_ro ? EROFS : 0);
            failed |= expect_access(alias_file, 0, EROFS);
            failed |= expect_mode(source_file, X_OK, 0, 0);
            failed |= expect_mode(alias_file, X_OK, 0, EACCES);
            failed |= check_mount_reports(source, 0, fs_ro);
            failed |= check_mount_reports(alias, 1, fs_ro);
            if (write(reply[1], &token, 1) != 1)
                _exit(5);
        }
        _exit(failed);
    }
    close(command[0]);
    command[0] = -1;
    close(reply[1]);
    reply[1] = -1;
    char token;
    if (read(reply[0], &token, 1) != 1)
        goto cleanup;
    for (int phase = 0; phase < 3; phase++) {
        if (phase && mount(NULL, source, NULL, MS_REMOUNT | (phase == 1 ? MS_RDONLY : 0), NULL))
            goto cleanup;
        token = (char)phase;
        if (write(command[1], &token, 1) != 1 || read(reply[0], &token, 1) != 1)
            goto cleanup;
    }
    int status;
    if (waitpid(child, &status, 0) != child)
        goto cleanup;
    child = -1;
    if (WIFEXITED(status) && !WEXITSTATUS(status)) {
        puts("ACCESS_READONLY_SCOPES_ACROSS_NAMESPACES_PASSED");
        result = 0;
    }
cleanup:
    if (result)
        perror("readonly access context");
    if (child > 0) {
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
    }
    for (int i = 0; i < 2; i++) {
        if (command[i] >= 0)
            close(command[i]);
        if (reply[i] >= 0)
            close(reply[i]);
    }
    if (bound)
        umount2(alias, MNT_DETACH);
    if (mounted)
        umount2(source, MNT_DETACH);
    if (created) {
        if (alias[0])
            rmdir(alias);
        if (source[0])
            rmdir(source);
        rmdir(directory);
    }
    return result;
}

int main(void)
{
    struct passwd *user = getpwnam("nobody");
    if (!user)
        return 1;
    int credentials = check_credentials(user->pw_uid);
    int search = check_search_paths(user->pw_uid);
    int readonly = check_readonly_scopes(user->pw_uid);
    return credentials || search || readonly;
}
