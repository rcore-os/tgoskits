#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <utime.h>
#include <unistd.h>

int main(void)
{
    TEST_START("SyscallGuard final umount and read-only utime behavior");

    char base[PATH_MAX];
    char mountpoint[PATH_MAX];
    char mounted_file[PATH_MAX];
    snprintf(base, sizeof(base), "/tmp/syscallguard-final-mount-%ld", (long)getpid());
    int ret = snprintf(mountpoint, sizeof(mountpoint), "%s/mnt", base);
    CHECK(ret > 0 && (size_t)ret < sizeof(mountpoint), "build mountpoint path");
    ret = snprintf(mounted_file, sizeof(mounted_file), "%s/file", mountpoint);
    CHECK(ret > 0 && (size_t)ret < sizeof(mounted_file), "build mounted file path");
    mkdir(base, 0755);
    mkdir(mountpoint, 0755);

    CHECK_ERR(umount("/tmp/syscallguard-final-does-not-exist"), ENOENT,
              "umount missing path returns ENOENT");
    char too_long[PATH_MAX + 32];
    memset(too_long, 'm', sizeof(too_long) - 1);
    too_long[sizeof(too_long) - 1] = '\0';
    CHECK_ERR(umount(too_long), ENAMETOOLONG,
              "umount overlong path returns ENAMETOOLONG");

    CHECK_RET(mount("none", mountpoint, "tmpfs", 0, NULL), 0,
              "mount tmpfs fixture");
    int fd = open(mounted_file, O_CREAT | O_RDWR | O_TRUNC, 0644);
    CHECK(fd >= 0, "create file inside mounted fixture");
    if (fd >= 0)
        close(fd);

    char old_cwd[PATH_MAX];
    CHECK(getcwd(old_cwd, sizeof(old_cwd)) != NULL, "capture cwd");
    CHECK_RET(chdir(mountpoint), 0, "enter mounted filesystem");
    CHECK_ERR(umount(mountpoint), EBUSY, "umount busy cwd returns EBUSY");
    CHECK_RET(chdir(old_cwd), 0, "leave mounted filesystem");

    CHECK_RET(mount("none", mountpoint, "tmpfs", MS_REMOUNT | MS_RDONLY, NULL), 0,
              "remount fixture read-only");
    CHECK_ERR(utime(mounted_file, NULL), EROFS,
              "utime on read-only mount returns EROFS");
    CHECK_RET(umount(mountpoint), 0, "unmount read-only fixture");

    rmdir(mountpoint);
    rmdir(base);
    TEST_DONE();
}
