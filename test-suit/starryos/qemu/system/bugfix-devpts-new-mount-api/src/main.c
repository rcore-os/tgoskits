#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/vfs.h>
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
#define MOVE_MOUNT_F_EMPTY_PATH 0x00000004

#define DEVPTS_SUPER_MAGIC 0x1cd1
#define MOUNT_PATH "/tmp/bugfix-devpts-new-mount-api"

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

static void set_string_option(int fsfd, const char *key, const char *value,
                              const char *message)
{
    errno = 0;
    check(syscall(SYS_fsconfig, fsfd, FSCONFIG_SET_STRING, key, value, 0) == 0,
          message);
}

static void verify_devpts_mount(void)
{
    struct statfs filesystem;
    struct stat metadata;

    errno = 0;
    int result = statfs(MOUNT_PATH, &filesystem);
    check(result == 0, "statfs reports the attached devpts mount");
    if (result == 0) {
        check((unsigned long)filesystem.f_type == DEVPTS_SUPER_MAGIC,
              "statfs exposes the devpts filesystem identity");
    }

    errno = 0;
    result = stat(MOUNT_PATH "/ptmx", &metadata);
    check(result == 0, "devpts exposes its ptmx node");
    if (result == 0) {
        check((metadata.st_mode & 07777) == 0666,
              "ptmxmode applies through the new mount API");
    }

    int master = open(MOUNT_PATH "/ptmx", O_RDWR | O_NOCTTY);
    check(master >= 0, "open allocates a PTY through the new devpts instance");
    if (master < 0) {
        return;
    }

    unsigned int number = ~0U;
    errno = 0;
    check(ioctl(master, TIOCGPTN, &number) == 0,
          "TIOCGPTN reports the allocated PTY number");

    char slave_path[128];
    int length =
        snprintf(slave_path, sizeof(slave_path), "%s/%u", MOUNT_PATH, number);
    check(length > 0 && (size_t)length < sizeof(slave_path),
          "format the allocated slave path");
    if (length > 0 && (size_t)length < sizeof(slave_path)) {
        errno = 0;
        result = stat(slave_path, &metadata);
        check(result == 0, "the allocated slave appears in the new instance");
        if (result == 0) {
            check((metadata.st_mode & 07777) == 0620,
                  "mode applies through the new mount API");
            check(metadata.st_gid == 5,
                  "gid applies through the new mount API");
        }
    }
    close(master);
}

int main(void)
{
    if (mkdir(MOUNT_PATH, 0755) != 0 && errno != EEXIST) {
        check(0, "create the devpts mountpoint");
    }

    errno = 0;
    int fsfd = syscall(SYS_fsopen, "devpts", FSOPEN_CLOEXEC);
    check(fsfd >= 0, "fsopen creates a devpts filesystem context");
    if (fsfd >= 0) {
        set_string_option(fsfd, "source", "devpts",
                          "fsconfig records the devpts mount source");
        set_string_option(fsfd, "mode", "0620",
                          "fsconfig accepts the devpts slave mode");
        set_string_option(fsfd, "gid", "5",
                          "fsconfig accepts the devpts slave gid");
        set_string_option(fsfd, "ptmxmode", "0666",
                          "fsconfig accepts the devpts ptmx mode");

        errno = 0;
        check(syscall(SYS_fsconfig, fsfd, FSCONFIG_SET_FLAG, "newinstance",
                      NULL, 0) == 0,
              "fsconfig accepts the devpts newinstance flag");

        errno = 0;
        check(syscall(SYS_fsconfig, fsfd, FSCONFIG_CMD_CREATE, NULL, NULL, 0) ==
                  0,
              "fsconfig creates the configured devpts instance");

        int mountfd = -1;
        if (failures == 0) {
            errno = 0;
            mountfd = syscall(SYS_fsmount, fsfd, FSMOUNT_CLOEXEC, 0);
            check(mountfd >= 0, "fsmount creates a detached devpts mount");
        }
        if (mountfd >= 0) {
            errno = 0;
            check(syscall(SYS_move_mount, mountfd, "", AT_FDCWD, MOUNT_PATH,
                          MOVE_MOUNT_F_EMPTY_PATH) == 0,
                  "move_mount attaches the devpts mount");
            if (failures == 0) {
                verify_devpts_mount();
            }
            close(mountfd);
        }
        close(fsfd);
    }

    if (syscall(SYS_umount2, MOUNT_PATH, 0) == 0) {
        rmdir(MOUNT_PATH);
    }

    if (failures != 0) {
        fprintf(stderr, "STARRY_DEVPTS_NEW_MOUNT_API_FAILED: %d checks\n",
                failures);
        return 1;
    }

    puts("STARRY_DEVPTS_NEW_MOUNT_API_PASSED");
    return 0;
}
