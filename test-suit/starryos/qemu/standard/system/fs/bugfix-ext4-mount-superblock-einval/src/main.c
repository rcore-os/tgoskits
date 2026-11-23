/*
 * bugfix-ext4-mount-superblock-einval: mount(2) of an ext4 device whose
 * superblock is refused must fail with EINVAL, not EIO.
 *
 * Linux behavior: ext4_load_super() in fs/ext4/super.c starts from -EINVAL
 * and keeps it both when s_magic is not 0xEF53 and when s_log_block_size is
 * larger than ext4 allows ("Invalid log block size").
 * StarryOS bug: rsext4 mapped every error from reading the superblock to Io,
 * so a superblock with the ext4 magic and a block size of 128 KiB was
 * reported as EIO, the same as a failed device read.
 *
 * The loop device reports 512-byte sectors, so both cases reach the
 * superblock checks rather than the device geometry check.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/syscall.h>
#include <unistd.h>

/* Linux include/uapi/linux/loop.h ABI. The cross libc sysroot does not ship
 * Linux headers. */
#define LOOP_SET_FD 0x4c00
#define LOOP_CLR_FD 0x4c01
#define LOOP_GET_STATUS64 0x4c05
struct loop_info64 {
    uint64_t lo_device, lo_inode, lo_rdevice, lo_offset, lo_sizelimit;
    uint32_t lo_number, lo_encrypt_type, lo_encrypt_key_size, lo_flags;
    uint8_t lo_file_name[64], lo_crypt_name[64], lo_encrypt_key[32];
    uint64_t lo_init[2];
};
_Static_assert(sizeof(struct loop_info64) == 232, "Linux loop_info64 ABI");

/* Linux Documentation/filesystems/ext4/super.rst: the primary superblock is
 * 1024 bytes at byte 1024; s_log_block_size is at 0x18, s_magic at 0x38. */
#define SUPERBLOCK_OFFSET 1024
#define SUPERBLOCK_SIZE 1024
#define S_LOG_BLOCK_SIZE 0x18
#define S_MAGIC 0x38
#define IMAGE_SIZE (64 * 1024)

static int failed;

static int open_free_loop(char *path, size_t path_len)
{
    for (int i = 0; i < 16; i++) {
        struct loop_info64 info;
        snprintf(path, path_len, "/dev/loop%d", i);
        int fd = open(path, O_RDWR | O_CLOEXEC);
        if (fd < 0)
            continue;
        errno = 0;
        if (ioctl(fd, LOOP_GET_STATUS64, &info) == -1 && errno == ENXIO)
            return fd;
        close(fd);
    }
    return -1;
}

static int write_superblock(int image_fd, uint32_t log_block_size, uint16_t magic)
{
    uint8_t superblock[SUPERBLOCK_SIZE] = {0};
    superblock[S_LOG_BLOCK_SIZE] = log_block_size & 0xff;
    superblock[S_LOG_BLOCK_SIZE + 1] = (log_block_size >> 8) & 0xff;
    superblock[S_LOG_BLOCK_SIZE + 2] = (log_block_size >> 16) & 0xff;
    superblock[S_LOG_BLOCK_SIZE + 3] = (log_block_size >> 24) & 0xff;
    superblock[S_MAGIC] = magic & 0xff;
    superblock[S_MAGIC + 1] = magic >> 8;
    return pwrite(image_fd, superblock, sizeof(superblock), SUPERBLOCK_OFFSET)
        == (ssize_t)sizeof(superblock) && fsync(image_fd) == 0 ? 0 : -1;
}

static void expect_einval(const char *loop, const char *target, const char *what)
{
    static const unsigned long flags[] = {0, MS_RDONLY};
    for (size_t i = 0; i < sizeof(flags) / sizeof(flags[0]); i++) {
        const char *mode = flags[i] ? "read-only" : "read-write";
        errno = 0;
        long result = syscall(SYS_mount, loop, target, "ext4", flags[i], NULL);
        int error = errno;
        if (result == -1 && error == EINVAL) {
            printf("PASS: %s mount of %s fails with EINVAL\n", mode, what);
            continue;
        }
        if (result == 0) {
            printf("FAIL: %s mount of %s succeeded\n", mode, what);
            syscall(SYS_umount2, target, 0);
        } else {
            printf("FAIL: %s mount of %s errno=%d (%s), expected EINVAL (%d)\n",
                   mode, what, error, strerror(error), EINVAL);
        }
        failed++;
    }
}

int main(void)
{
    char image[] = "/tmp/ext4-superblock-einval.XXXXXX";
    char target[] = "/tmp/ext4-superblock-einval-mnt.XXXXXX";
    char loop[32];
    int image_fd = -1, loop_fd = -1, bound = 0, made_target = 0;

    printf("=== bugfix-ext4-mount-superblock-einval ===\n");

    image_fd = mkstemp(image);
    if (image_fd < 0 || ftruncate(image_fd, IMAGE_SIZE) != 0) {
        printf("FAIL: cannot create image: %s\n", strerror(errno));
        failed++;
        goto cleanup;
    }
    if (mkdtemp(target) == NULL) {
        printf("FAIL: cannot create mount point: %s\n", strerror(errno));
        failed++;
        goto cleanup;
    }
    made_target = 1;
    loop_fd = open_free_loop(loop, sizeof(loop));
    if (loop_fd < 0) {
        printf("FAIL: no free loop device\n");
        failed++;
        goto cleanup;
    }
    if (ioctl(loop_fd, LOOP_SET_FD, image_fd) != 0) {
        printf("FAIL: LOOP_SET_FD %s: %s\n", loop, strerror(errno));
        failed++;
        goto cleanup;
    }
    bound = 1;

    /* No ext4 filesystem: every superblock byte is zero. */
    expect_einval(loop, target, "a device without the ext4 magic");

    /* The ext4 magic with a 1024 << 7 = 128 KiB block size. */
    if (write_superblock(image_fd, 7, 0xEF53) != 0) {
        printf("FAIL: cannot write superblock: %s\n", strerror(errno));
        failed++;
        goto cleanup;
    }
    expect_einval(loop, target, "a superblock with a 128 KiB block size");

cleanup:
    if (bound && ioctl(loop_fd, LOOP_CLR_FD) != 0) {
        printf("FAIL: LOOP_CLR_FD %s: %s\n", loop, strerror(errno));
        failed++;
    }
    if (loop_fd >= 0)
        close(loop_fd);
    if (image_fd >= 0) {
        close(image_fd);
        unlink(image);
    }
    if (made_target)
        rmdir(target);

    if (failed) {
        printf("TEST FAILED\n");
        return 1;
    }
    printf("TEST PASSED\n");
    return 0;
}
