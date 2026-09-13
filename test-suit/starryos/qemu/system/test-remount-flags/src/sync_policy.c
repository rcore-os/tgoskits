#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <sys/statvfs.h>
#include <sys/syscall.h>
#include <unistd.h>

static bool has_option(char *options, const char *wanted) {
    char *saved = NULL;
    for (char *option = strtok_r(options, ",", &saved); option;
         option = strtok_r(NULL, ",", &saved)) {
        if (strcmp(option, wanted) == 0)
            return true;
    }
    return false;
}

static int check_mountinfo(const char *path, bool synchronous) {
    FILE *file = fopen("/proc/self/mountinfo", "r");
    if (!file) {
        perror("open mountinfo for sync policy");
        return 1;
    }
    char *line = NULL;
    size_t capacity = 0;
    int failed = 1;
    while (getline(&line, &capacity, file) >= 0) {
        char mountpoint[256], options[256], directory_options[256];
        if (sscanf(line, "%*s %*s %*s %*s %255s", mountpoint) != 1 ||
            strcmp(mountpoint, path) != 0)
            continue;
        char *separator = strstr(line, " - ");
        if (!separator || sscanf(separator + 3, "%*s %*s %255s", options) != 1)
            break;
        strcpy(directory_options, options);
        if (has_option(options, "sync") != synchronous ||
            !has_option(directory_options, "dirsync")) {
            fprintf(stderr, "FAIL: sync/dirsync mountinfo policy: %s", line);
            break;
        }
        failed = 0;
        break;
    }
    if (failed)
        fprintf(stderr, "FAIL: expected sync=%d and dirsync for %s\n", synchronous, path);
    free(line);
    fclose(file);
    return failed;
}

static int check_sync_policy(int fd, const char *path, bool synchronous) {
    struct statfs by_fd, by_path;
    if (syscall(SYS_fstatfs, fd, &by_fd) != 0 ||
        syscall(SYS_statfs, path, &by_path) != 0) {
        perror("statfs sync policy");
        return 1;
    }
    if (!!(by_fd.f_flags & ST_SYNCHRONOUS) != synchronous ||
        !!(by_path.f_flags & ST_SYNCHRONOUS) != synchronous) {
        fprintf(stderr, "FAIL: %s sync=%d, fstatfs=%lx statfs=%lx\n",
                path, synchronous, (unsigned long)by_fd.f_flags,
                (unsigned long)by_path.f_flags);
        return 1;
    }
    return check_mountinfo(path, synchronous);
}

int verify_mount_sync_policy(void) {
    char directory[] = "/tmp/remount-sync-XXXXXX";
    char source[256], alias[256];
    bool source_mounted = false, alias_mounted = false;
    int source_fd = -1, alias_fd = -1;
    int failed = 1;

    if (!mkdtemp(directory)) {
        perror("create sync-policy test directory");
        return 1;
    }
    snprintf(source, sizeof(source), "%s/source", directory);
    snprintf(alias, sizeof(alias), "%s/alias", directory);
    if (mkdir(source, 0700) != 0 || mkdir(alias, 0700) != 0) {
        perror("create sync-policy mountpoints");
        goto cleanup;
    }
    if (syscall(SYS_mount, "none", source, "tmpfs", MS_DIRSYNC, "size=1m") != 0) {
        perror("mount dirsync tmpfs");
        goto cleanup;
    }
    source_mounted = true;
    if (syscall(SYS_mount, source, alias, NULL, MS_BIND, NULL) != 0) {
        perror("bind sync-policy mount");
        goto cleanup;
    }
    alias_mounted = true;
    source_fd = open(source, O_RDONLY | O_DIRECTORY);
    alias_fd = open(alias, O_RDONLY | O_DIRECTORY);
    if (source_fd < 0 || alias_fd < 0) {
        perror("open mounts before remount");
        goto cleanup;
    }
    if (check_sync_policy(source_fd, source, false) ||
        check_sync_policy(alias_fd, alias, false))
        goto cleanup;

    if (syscall(SYS_mount, NULL, source, NULL,
                MS_REMOUNT | MS_SYNCHRONOUS, NULL) != 0) {
        perror("enable shared synchronous policy");
        goto cleanup;
    }
    if (check_sync_policy(source_fd, source, true) ||
        check_sync_policy(alias_fd, alias, true))
        goto cleanup;

    /* A bind remount changes the mount, not the shared superblock policy. */
    if (syscall(SYS_mount, NULL, alias, NULL, MS_REMOUNT | MS_BIND, NULL) != 0) {
        perror("bind remount preserves superblock sync");
        goto cleanup;
    }
    if (check_sync_policy(source_fd, source, true) ||
        check_sync_policy(alias_fd, alias, true))
        goto cleanup;

    /* MS_DIRSYNC is not in Linux's remount-changeable superblock mask. */
    if (syscall(SYS_mount, NULL, source, NULL, MS_REMOUNT, NULL) != 0) {
        perror("disable synchronous policy");
        goto cleanup;
    }
    if (check_sync_policy(source_fd, source, false) ||
        check_sync_policy(alias_fd, alias, false))
        goto cleanup;
    failed = 0;

cleanup:
    if (alias_fd >= 0)
        close(alias_fd);
    if (source_fd >= 0)
        close(source_fd);
    if (alias_mounted && syscall(SYS_umount2, alias, 0) != 0) {
        perror("unmount sync-policy alias");
        failed = 1;
    }
    if (source_mounted && syscall(SYS_umount2, source, 0) != 0) {
        perror("unmount sync-policy source");
        failed = 1;
    }
    if (rmdir(alias) != 0 && errno != ENOENT) {
        perror("remove sync-policy alias directory");
        failed = 1;
    }
    if (rmdir(source) != 0 && errno != ENOENT) {
        perror("remove sync-policy source directory");
        failed = 1;
    }
    if (rmdir(directory) != 0) {
        perror("remove sync-policy test directory");
        failed = 1;
    }
    return failed;
}
