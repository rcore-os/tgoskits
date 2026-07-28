#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <unistd.h>

static int failures;

static void pass(const char *message)
{
    printf("  PASS: %s\n", message);
}

static void fail(const char *message)
{
    printf("  FAIL: %s (errno=%d: %s)\n", message, errno, strerror(errno));
    failures++;
}

static int mount_devpts(const char *path)
{
    if (mkdir(path, 0755) != 0 && errno != EEXIST) {
        fail("create devpts mountpoint");
        return -1;
    }
    if (mount("none", path, "devpts", 0,
              "newinstance,mode=0620,gid=5,ptmxmode=0666") != 0) {
        fail("mount private devpts instance");
        return -1;
    }
    return 0;
}

static void check_metadata(const char *path, mode_t mode, gid_t gid,
                           const char *message)
{
    struct stat metadata;
    if (stat(path, &metadata) != 0) {
        fail(message);
        return;
    }
    if ((metadata.st_mode & 07777) != mode || metadata.st_gid != gid) {
        errno = EPROTO;
        fail(message);
        return;
    }
    pass(message);
}

static int allocate_pty(const char *mountpoint, unsigned int *number)
{
    char path[128];
    int length = snprintf(path, sizeof(path), "%s/ptmx", mountpoint);
    if (length < 0 || (size_t)length >= sizeof(path)) {
        errno = ENAMETOOLONG;
        fail("format ptmx path");
        return -1;
    }

    int master = open(path, O_RDWR | O_NOCTTY);
    if (master < 0) {
        fail("open per-instance ptmx");
        return -1;
    }
    if (ioctl(master, TIOCGPTN, number) != 0) {
        fail("read allocated PTY number");
        close(master);
        return -1;
    }
    return master;
}

static int slave_exists(const char *mountpoint, unsigned int number)
{
    char path[128];
    int length = snprintf(path, sizeof(path), "%s/%u", mountpoint, number);
    if (length < 0 || (size_t)length >= sizeof(path)) {
        errno = ENAMETOOLONG;
        return -1;
    }

    struct stat metadata;
    return stat(path, &metadata);
}

int main(void)
{
    static const char first_mount[] = "/tmp/devpts-newinstance-a";
    static const char second_mount[] = "/tmp/devpts-newinstance-b";

    if (mount_devpts(first_mount) != 0 || mount_devpts(second_mount) != 0) {
        return 1;
    }

    check_metadata("/tmp/devpts-newinstance-a/ptmx", 0666, 0,
                   "ptmxmode applies to the per-instance ptmx node");

    unsigned int first_number = ~0U;
    int first_master = allocate_pty(first_mount, &first_number);
    if (first_master < 0) {
        return 1;
    }
    if (first_number != 0) {
        errno = EPROTO;
        fail("first devpts instance starts PTY numbering at zero");
    } else {
        pass("first devpts instance starts PTY numbering at zero");
    }
    if (slave_exists(first_mount, first_number) != 0) {
        fail("allocated slave is visible in its owning devpts instance");
    } else {
        pass("allocated slave is visible in its owning devpts instance");
    }
    check_metadata("/tmp/devpts-newinstance-a/0", 0620, 5,
                   "mode and gid apply to the allocated slave node");

    errno = 0;
    if (slave_exists(second_mount, first_number) == 0 || errno != ENOENT) {
        errno = EPROTO;
        fail("slave from first instance is hidden from second instance");
    } else {
        pass("slave from first instance is hidden from second instance");
    }

    unsigned int second_number = ~0U;
    int second_master = allocate_pty(second_mount, &second_number);
    if (second_master < 0) {
        close(first_master);
        return 1;
    }
    if (second_number != 0) {
        errno = EPROTO;
        fail("second devpts instance has an independent PTY index space");
    } else {
        pass("second devpts instance has an independent PTY index space");
    }
    if (slave_exists(second_mount, second_number) != 0) {
        fail("second instance exposes its own allocated slave");
    } else {
        pass("second instance exposes its own allocated slave");
    }

    close(second_master);
    close(first_master);
    umount2(second_mount, MNT_DETACH);
    umount2(first_mount, MNT_DETACH);
    rmdir(second_mount);
    rmdir(first_mount);

    if (failures != 0) {
        printf("TEST_DEVPTS_NEWINSTANCE_FAILED failures=%d\n", failures);
        return 1;
    }
    puts("TEST_DEVPTS_NEWINSTANCE_PASSED");
    return 0;
}
