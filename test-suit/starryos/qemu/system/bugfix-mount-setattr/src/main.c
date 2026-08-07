#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/sysinfo.h>
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
#ifndef SYS_mount_setattr
#define SYS_mount_setattr 442
#endif

#define FSOPEN_CLOEXEC 0x00000001
#define FSMOUNT_CLOEXEC 0x00000001
#define FSCONFIG_SET_STRING 1
#define FSCONFIG_CMD_CREATE 6
#define MOVE_MOUNT_F_EMPTY_PATH 0x00000004
#define AT_EMPTY_PATH 0x1000

#define MOUNT_ATTR_NOSUID 0x00000002
#define MOUNT_ATTR_NODEV 0x00000004
#define MOUNT_ATTR__ATIME 0x00000070
#define MOUNT_ATTR_STRICTATIME 0x00000020

#ifndef ST_NOSUID
#define ST_NOSUID 0x00000002
#endif
#ifndef ST_NODEV
#define ST_NODEV 0x00000004
#endif
#define MOUNT_PATH "/tmp/bugfix-mount-setattr"
#define TMPFS_MOUNT_PATH "/tmp/bugfix-mount-setattr-tmpfs"

struct mount_attr {
    uint64_t attr_set;
    uint64_t attr_clr;
    uint64_t propagation;
    uint64_t userns_fd;
};

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

static void check_syscall_availability_probe(void)
{
    errno = 0;
    check(syscall(SYS_mount_setattr, -1, NULL, 0, NULL, 0) == 0,
          "mount_setattr accepts the util-linux availability probe");
}

static int create_detached_ramfs(void)
{
    int fsfd = syscall(SYS_fsopen, "ramfs", FSOPEN_CLOEXEC);
    check(fsfd >= 0, "fsopen creates a ramfs filesystem context");
    if (fsfd < 0) {
        return -1;
    }

    errno = 0;
    check(syscall(SYS_fsconfig, fsfd, FSCONFIG_SET_STRING, "source", "ramfs",
                  0) == 0,
          "fsconfig records the ramfs mount source");
    errno = 0;
    check(syscall(SYS_fsconfig, fsfd, FSCONFIG_SET_STRING, "mode", "0750", 0) ==
              0,
          "fsconfig sets the ramfs root mode");
    errno = 0;
    check(syscall(SYS_fsconfig, fsfd, FSCONFIG_CMD_CREATE, NULL, NULL, 0) == 0,
          "fsconfig creates the ramfs");

    int mountfd = -1;
    if (failures == 0) {
        mountfd = syscall(SYS_fsmount, fsfd, FSMOUNT_CLOEXEC, 0);
        check(mountfd >= 0, "fsmount creates a detached ramfs mount");
    }
    close(fsfd);
    return mountfd;
}

static int create_detached_sized_tmpfs(void)
{
    int fsfd = syscall(SYS_fsopen, "tmpfs", FSOPEN_CLOEXEC);
    check(fsfd >= 0, "fsopen creates a tmpfs filesystem context");
    if (fsfd < 0) {
        return -1;
    }

    errno = 0;
    check(syscall(SYS_fsconfig, fsfd, FSCONFIG_SET_STRING, "source", "tmpfs",
                  0) == 0,
          "fsconfig records the tmpfs mount source");
    errno = 0;
    check(syscall(SYS_fsconfig, fsfd, FSCONFIG_SET_STRING, "mode", "0755", 0) ==
              0,
          "fsconfig sets the tmpfs root mode");
    errno = 0;
    check(syscall(SYS_fsconfig, fsfd, FSCONFIG_SET_STRING, "size", "25%", 0) ==
              0,
          "fsconfig accepts the NixOS /run tmpfs size");
    errno = 0;
    check(syscall(SYS_fsconfig, fsfd, FSCONFIG_CMD_CREATE, NULL, NULL, 0) == 0,
          "fsconfig creates the sized NixOS /run tmpfs");

    int mountfd = -1;
    if (failures == 0) {
        mountfd = syscall(SYS_fsmount, fsfd, FSMOUNT_CLOEXEC, 0);
        check(mountfd >= 0, "fsmount creates the sized detached tmpfs");
    }
    close(fsfd);
    return mountfd;
}

static void apply_mount_attributes(int mountfd)
{
    struct mount_attr attributes = {
        .attr_set =
            MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV | MOUNT_ATTR_STRICTATIME,
        .attr_clr = MOUNT_ATTR__ATIME,
    };

    errno = 0;
    check(syscall(SYS_mount_setattr, mountfd, "", AT_EMPTY_PATH, &attributes,
                  sizeof(attributes) - 1) == -1 &&
              errno == EINVAL,
          "mount_setattr rejects a short mount_attr structure");

    errno = 0;
    check(syscall(SYS_mount_setattr, mountfd, "", AT_EMPTY_PATH << 1,
                  &attributes, sizeof(attributes)) == -1 &&
              errno == EINVAL,
          "mount_setattr rejects unknown lookup flags");

    errno = 0;
    check(syscall(SYS_mount_setattr, mountfd, "", AT_EMPTY_PATH, &attributes,
                  sizeof(attributes)) == 0,
          "mount_setattr applies VFS attributes to a detached mount");
}

static void attach_and_verify(int mountfd)
{
    struct statfs filesystem;

    errno = 0;
    check(syscall(SYS_move_mount, mountfd, "", AT_FDCWD, MOUNT_PATH,
                  MOVE_MOUNT_F_EMPTY_PATH) == 0,
          "move_mount attaches the attributed mount");

    errno = 0;
    int result = statfs(MOUNT_PATH, &filesystem);
    check(result == 0, "statfs reports the attached mount");
    if (result == 0) {
        unsigned long required = ST_NOSUID | ST_NODEV;
        check(((unsigned long)filesystem.f_flags & required) == required,
              "statfs exposes nosuid and nodev");
    }
}

int main(void)
{
    check_syscall_availability_probe();

    if (mkdir(TMPFS_MOUNT_PATH, 0755) != 0 && errno != EEXIST) {
        check(0, "create the sized tmpfs mountpoint");
    } else {
        int tmpfs_mountfd = create_detached_sized_tmpfs();
        if (tmpfs_mountfd >= 0) {
            errno = 0;
            check(syscall(SYS_move_mount, tmpfs_mountfd, "", AT_FDCWD,
                          TMPFS_MOUNT_PATH, MOVE_MOUNT_F_EMPTY_PATH) == 0,
                  "move_mount attaches the sized tmpfs");
            struct statfs filesystem;
            struct sysinfo memory;
            errno = 0;
            int stat_result = statfs(TMPFS_MOUNT_PATH, &filesystem);
            check(stat_result == 0, "statfs reports the sized tmpfs");
            errno = 0;
            int sysinfo_result = sysinfo(&memory);
            check(sysinfo_result == 0, "sysinfo reports total memory");
            if (stat_result == 0 && sysinfo_result == 0) {
                uint64_t total_bytes =
                    (uint64_t)memory.totalram * memory.mem_unit;
                uint64_t expected_blocks =
                    ((total_bytes / 4) + filesystem.f_bsize - 1) /
                    filesystem.f_bsize;
                check((uint64_t)filesystem.f_blocks == expected_blocks,
                      "statfs preserves the 25% tmpfs size limit");
            }
            close(tmpfs_mountfd);
        }
        if (syscall(SYS_umount2, TMPFS_MOUNT_PATH, 0) == 0) {
            rmdir(TMPFS_MOUNT_PATH);
        }
    }

    if (mkdir(MOUNT_PATH, 0755) != 0 && errno != EEXIST) {
        check(0, "create the mountpoint");
    } else {
        int mountfd = create_detached_ramfs();
        if (mountfd >= 0) {
            apply_mount_attributes(mountfd);
            if (failures == 0) {
                attach_and_verify(mountfd);
            }
            close(mountfd);
        }

        if (syscall(SYS_umount2, MOUNT_PATH, 0) == 0) {
            rmdir(MOUNT_PATH);
        }
    }

    if (failures != 0) {
        fprintf(stderr, "STARRY_MOUNT_SETATTR_FAILED: %d checks\n", failures);
        return 1;
    }

    puts("STARRY_MOUNT_SETATTR_PASSED");
    return 0;
}
