#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/loop.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <sys/syscall.h>
#include <unistd.h>

static unsigned char payload[1024 * 1024];
static unsigned char actual[sizeof(payload)];
static const char *mountpoint = "/mnt/fragmented";
static const char *filename = "/mnt/fragmented/payload";

static void fail(const char *operation)
{
    fprintf(stderr, "EXT4_FRAGMENTED_WRITEBACK_FAILED: %s: %s\n", operation, strerror(errno));
    exit(1);
}

static void verify(void)
{
    int fd = open(filename, O_RDONLY);
    if (fd < 0)
        fail("open for verification");
    for (unsigned int part = 0; part < 2; part++) {
        ssize_t count = syscall(SYS_read, fd, actual, sizeof(actual));
        if (count != (ssize_t)sizeof(actual) || memcmp(actual, payload, sizeof(actual))) {
            errno = EIO;
            fail("persisted payload mismatch");
        }
    }
    if (close(fd))
        fail("close reader");
}

int main(int argc, char **argv)
{
    if (argc != 2) {
        fprintf(stderr, "usage: fragmented-writeback IMAGE\n");
        return 1;
    }
    int image = open(argv[1], O_RDWR);
    if (image < 0)
        fail("open fixture image");
    // This test owns a fresh, disposable VM; refuse to replace a bound loop0.
    const char *device = "/dev/loop0";
    int loop = open(device, O_RDWR);
    if (loop < 0 || ioctl(loop, LOOP_SET_FD, image))
        fail("attach loop image");
    if (mkdir("/mnt", 0755) && errno != EEXIST)
        fail("mkdir /mnt");
    if (mkdir(mountpoint, 0755) && errno != EEXIST)
        fail("mkdir mountpoint");
    if (syscall(SYS_mount, device, mountpoint, "ext4", 0, NULL))
        fail("mount fixture");
    struct statfs space;
    if (statfs(mountpoint, &space))
        fail("statfs fixture");
    if ((unsigned long long)space.f_bfree * space.f_bsize < 16 * sizeof(payload)) {
        errno = ENOSPC;
        fail("fixture must have ample free space");
    }
    printf("fragmented_free_bytes=%llu\n", (unsigned long long)space.f_bfree * space.f_bsize);
    fflush(stdout);
    for (size_t i = 0; i < sizeof(payload); i++)
        payload[i] = (unsigned char)(i * 17 + i / 4096);
    int fd = open(filename, O_RDWR | O_CREAT | O_TRUNC, 0600);
    if (fd < 0)
        fail("create payload");
    for (unsigned int part = 0; part < 2; part++) {
        ssize_t count = syscall(SYS_write, fd, payload, sizeof(payload));
        if (count != (ssize_t)sizeof(payload))
            fail("write payload");
        if (syscall(part ? SYS_fdatasync : SYS_fsync, fd))
            fail("sync payload");
    }
    if (close(fd))
        fail("close writer");
    verify();
    if (syscall(SYS_umount2, mountpoint, 0))
        fail("unmount after writeback");
    if (syscall(SYS_mount, device, mountpoint, "ext4", 0, NULL))
        fail("remount fixture");
    verify();
    if (syscall(SYS_umount2, mountpoint, 0))
        fail("final unmount");
    if (ioctl(loop, LOOP_CLR_FD))
        fail("detach loop image");
    close(loop);
    close(image);
    puts("EXT4_FRAGMENTED_WRITEBACK_PASSED");
    return 0;
}
