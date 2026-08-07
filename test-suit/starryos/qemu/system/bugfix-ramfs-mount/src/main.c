#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/vfs.h>
#include <unistd.h>

#define RAMFS_MAGIC 0x858458f6
#define RAMFS_MOUNT_PATH "/tmp/bugfix-ramfs-mount"
#define RAMFS_TEST_FILE RAMFS_MOUNT_PATH "/round-trip"

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

static int check_ramfs_mount(void)
{
    struct statfs filesystem;

    errno = 0;
    long result = syscall(SYS_mount, "none", RAMFS_MOUNT_PATH, "ramfs", 0,
                          NULL);
    check(result == 0, "mount(2) accepts the ramfs filesystem type");
    if (result != 0) {
        return 0;
    }

    errno = 0;
    result = statfs(RAMFS_MOUNT_PATH, &filesystem);
    check(result == 0, "statfs reports the mounted ramfs");
    if (result == 0) {
        check((unsigned long)filesystem.f_type == RAMFS_MAGIC,
              "ramfs exposes RAMFS_MAGIC instead of TMPFS_MAGIC");
    }
    return 1;
}

static void check_file_round_trip(void)
{
    static const char expected[] = "starrynixos-ramfs";
    char observed[sizeof(expected)] = {0};

    errno = 0;
    int fd = open(RAMFS_TEST_FILE, O_CREAT | O_EXCL | O_RDWR | O_CLOEXEC,
                  0600);
    check(fd >= 0, "create a regular file on ramfs");
    if (fd < 0) {
        return;
    }

    errno = 0;
    ssize_t length = write(fd, expected, sizeof(expected));
    check(length == (ssize_t)sizeof(expected), "write file content on ramfs");

    errno = 0;
    check(lseek(fd, 0, SEEK_SET) == 0, "seek to the start of a ramfs file");

    errno = 0;
    length = read(fd, observed, sizeof(observed));
    check(length == (ssize_t)sizeof(expected) &&
              memcmp(observed, expected, sizeof(expected)) == 0,
          "read back file content from ramfs");
    close(fd);
    unlink(RAMFS_TEST_FILE);
}

static void check_ramfs_unmount(void)
{
    errno = 0;
    check(syscall(SYS_umount2, RAMFS_MOUNT_PATH, 0) == 0,
          "umount2 removes the ramfs mount");
}

int main(void)
{
    if (mkdir(RAMFS_MOUNT_PATH, 0755) != 0 && errno != EEXIST) {
        check(0, "create the ramfs mountpoint");
    } else {
        int mounted = check_ramfs_mount();
        if (mounted) {
            check_file_round_trip();
            check_ramfs_unmount();
        }
        rmdir(RAMFS_MOUNT_PATH);
    }

    if (failures != 0) {
        fprintf(stderr, "STARRY_RAMFS_MOUNT_FAILED: %d checks\n", failures);
        return 1;
    }

    puts("STARRY_RAMFS_MOUNT_PASSED");
    return 0;
}
