#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_move_mount
#define SYS_move_mount 429
#endif
#ifndef SYS_fsopen
#define SYS_fsopen 430
#endif
#ifndef SYS_fsconfig
#define SYS_fsconfig 431
#endif
#ifndef SYS_fsmount
#define SYS_fsmount 432
#endif

#define FSOPEN_CLOEXEC 0x00000001
#define FSMOUNT_CLOEXEC 0x00000001
#define FSCONFIG_SET_FLAG 0
#define FSCONFIG_SET_STRING 1
#define FSCONFIG_CMD_CREATE 6
#define FSCONFIG_CMD_RECONFIGURE 7
#define MOVE_MOUNT_F_EMPTY_PATH 0x00000004
#define MOUNT_ATTR_NOSUID 0x00000002
#define MOUNT_ATTR_NODEV 0x00000004
#define MOUNT_ATTR_NOEXEC 0x00000008
#define MOUNT_ATTR_NOSYMFOLLOW 0x00200000

#define CREDENTIAL_MOUNT_PATH "/tmp/bugfix-new-mount-api-credentials"
#define CREDENTIAL_NAME "stage2-proof"

static int failures;

static void check(int condition, const char *message)
{
    if (condition) {
        printf("PASS: %s\n", message);
        return;
    }

    fprintf(stderr, "FAIL: %s: errno=%d (%s)\n", message, errno,
            strerror(errno));
    failures++;
}

static int configure_credentials_fs(void)
{
    int fsfd = syscall(SYS_fsopen, "tmpfs", FSOPEN_CLOEXEC);
    check(fsfd >= 0, "fsopen creates a tmpfs filesystem context");
    if (fsfd < 0) {
        return -1;
    }

    errno = 0;
    check(syscall(SYS_fsconfig, fsfd, FSCONFIG_SET_STRING, "nr_inodes",
                  "1024", 0) == 0,
          "fsconfig accepts the credential inode limit");
    errno = 0;
    check(syscall(SYS_fsconfig, fsfd, FSCONFIG_SET_STRING, "size", "1M", 0) ==
              0,
          "fsconfig accepts the credential size limit");
    errno = 0;
    long noswap =
        syscall(SYS_fsconfig, fsfd, FSCONFIG_SET_FLAG, "noswap", NULL, 0);
    check(noswap == 0 || (noswap < 0 && errno == EINVAL),
          "fsconfig either accepts noswap or requests the ramfs fallback");
    if (noswap < 0 && errno == EINVAL) {
        close(fsfd);
        fsfd = syscall(SYS_fsopen, "ramfs", FSOPEN_CLOEXEC);
        check(fsfd >= 0, "fsopen creates the systemd ramfs fallback context");
        if (fsfd < 0) {
            return -1;
        }
    }
    errno = 0;
    check(syscall(SYS_fsconfig, fsfd, FSCONFIG_SET_STRING, "mode", "0700", 0) ==
              0,
          "fsconfig accepts the credential root mode");
    errno = 0;
    check(syscall(SYS_fsconfig, fsfd, FSCONFIG_CMD_CREATE, NULL, NULL, 0) == 0,
          "fsconfig creates the configured filesystem");
    if (failures != 0) {
        close(fsfd);
        return -1;
    }
    return fsfd;
}

static int mount_credentials_fs(int fsfd)
{
    unsigned int attributes = MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV |
                              MOUNT_ATTR_NOEXEC | MOUNT_ATTR_NOSYMFOLLOW;
    int mfd = syscall(SYS_fsmount, fsfd, FSMOUNT_CLOEXEC, attributes);
    if (mfd < 0 && errno == EINVAL) {
        attributes &= ~MOUNT_ATTR_NOSYMFOLLOW;
        mfd = syscall(SYS_fsmount, fsfd, FSMOUNT_CLOEXEC, attributes);
    }
    check(mfd >= 0, "fsmount creates a detached credentials mount");
    return mfd;
}

static void populate_and_attach(int fsfd, int mfd)
{
    static const char expected[] = "starrynixos-stage2";
    char observed[sizeof(expected)] = {0};

    int dfd = openat(mfd, ".", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    check(dfd >= 0, "detached mount fd can be reopened as a directory");
    if (dfd >= 0) {
        int fd = openat(dfd, CREDENTIAL_NAME,
                        O_CREAT | O_EXCL | O_RDWR | O_CLOEXEC, 0400);
        check(fd >= 0, "credential file can be created before attachment");
        if (fd >= 0) {
            check(write(fd, expected, sizeof(expected)) ==
                      (ssize_t)sizeof(expected),
                  "credential content can be written before attachment");
            close(fd);
        }
        close(dfd);
    }

    errno = 0;
    check(syscall(SYS_fsconfig, fsfd, FSCONFIG_SET_FLAG, "ro", NULL, 0) == 0,
          "fsconfig accepts the read-only reconfiguration flag");
    errno = 0;
    check(syscall(SYS_fsconfig, fsfd, FSCONFIG_CMD_RECONFIGURE, NULL, NULL, 0) ==
              0,
          "fsconfig commits read-only reconfiguration");

    errno = 0;
    check(syscall(SYS_move_mount, mfd, "", AT_FDCWD, CREDENTIAL_MOUNT_PATH,
                  MOVE_MOUNT_F_EMPTY_PATH) == 0,
          "move_mount attaches the detached credentials mount");

    int fd = open(CREDENTIAL_MOUNT_PATH "/" CREDENTIAL_NAME,
                  O_RDONLY | O_CLOEXEC);
    check(fd >= 0, "attached credential file is visible by path");
    if (fd >= 0) {
        check(read(fd, observed, sizeof(observed)) == (ssize_t)sizeof(expected) &&
                  memcmp(observed, expected, sizeof(expected)) == 0,
              "attached credential content survives the move");
        close(fd);
    }

    errno = 0;
    fd = open(CREDENTIAL_MOUNT_PATH "/must-stay-read-only",
              O_CREAT | O_WRONLY | O_CLOEXEC, 0600);
    check(fd < 0 && errno == EROFS,
          "reconfigured credential mount rejects new writes");
    if (fd >= 0) {
        close(fd);
    }
}

int main(void)
{
    if (mkdir(CREDENTIAL_MOUNT_PATH, 0755) != 0 && errno != EEXIST) {
        check(0, "create the credentials mountpoint");
    } else {
        int fsfd = configure_credentials_fs();
        if (fsfd >= 0) {
            int mfd = mount_credentials_fs(fsfd);
            if (mfd >= 0) {
                populate_and_attach(fsfd, mfd);
                close(mfd);
            }
            close(fsfd);
        }

        if (syscall(SYS_umount2, CREDENTIAL_MOUNT_PATH, 0) == 0) {
            rmdir(CREDENTIAL_MOUNT_PATH);
        }
    }

    if (failures != 0) {
        fprintf(stderr, "STARRY_NEW_MOUNT_API_CREDENTIALS_FAILED: %d checks\n",
                failures);
        return 1;
    }

    puts("STARRY_NEW_MOUNT_API_CREDENTIALS_PASSED");
    return 0;
}
