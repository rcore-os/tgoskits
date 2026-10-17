#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mount.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

/* Linux v7.1 include/uapi/linux/loop.h ABI. The cross libc sysroot does
 * not ship Linux headers; these declarations contain no test implementation. */
#define LOOP_SET_FD 0x4c00
#define LOOP_CLR_FD 0x4c01
#define LOOP_SET_STATUS64 0x4c04
#define LOOP_GET_STATUS64 0x4c05
#define LO_FLAGS_AUTOCLEAR 4
/* Block-device ioctls are _IO(0x12,nr) on every architecture this suite runs. */
#ifndef BLKROSET
#define BLKROSET 0x125d   /* _IO(0x12,93) */
#endif
#ifndef BLKFLSBUF
#define BLKFLSBUF 0x1261  /* _IO(0x12,97) */
#endif
struct loop_info64 {
    uint64_t lo_device, lo_inode, lo_rdevice, lo_offset, lo_sizelimit;
    uint32_t lo_number, lo_encrypt_type, lo_encrypt_key_size, lo_flags;
    uint8_t lo_file_name[64], lo_crypt_name[64], lo_encrypt_key[32];
    uint64_t lo_init[2];
};
_Static_assert(sizeof(struct loop_info64) == 232, "Linux loop_info64 ABI");

/* LTP's parent must be able to unmount after waiting for a worker which left
 * cwd inside the mount. Pin both tasks to one CPU and give the parent FIFO
 * priority so it observes the published exit before the child's runtime can
 * finish retiring. There is no sleep or retry to hide delayed fs teardown. */
static int check_exit_fs(int private_fs)
{
    char target[] = "/tmp/ltp-exit-fs.XXXXXX";
    int ready[2], release[2];
    cpu_set_t affinity;
    struct sched_param priority = {.sched_priority = 30};
    CPU_ZERO(&affinity);
    CPU_SET(0, &affinity);
    if (syscall(SYS_sched_setaffinity, 0, sizeof(affinity), &affinity) ||
        syscall(SYS_sched_setscheduler, 0, SCHED_FIFO, &priority) ||
        !mkdtemp(target) || mount("none", target, "tmpfs", 0, NULL) ||
        pipe(ready) || pipe(release)) {
        perror("prepare exit-fs regression");
        return 1;
    }
    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return 1;
    }
    if (!child) {
        close(ready[0]);
        close(release[1]);
        priority.sched_priority = 0;
        if (syscall(SYS_sched_setscheduler, 0, SCHED_OTHER, &priority) ||
            (private_fs && syscall(SYS_unshare, CLONE_FS)) || chdir(target))
            _exit(2);
        char token = 'x';
        if (write(ready[1], &token, 1) != 1 || read(release[0], &token, 1) != 1)
            _exit(3);
        _exit(0);
    }
    close(ready[1]);
    close(release[0]);
    char token;
    int status;
    if (read(ready[0], &token, 1) != 1) {
        fprintf(stderr, "child did not publish its cwd\n");
        return 1;
    }
    int busy_result = syscall(SYS_umount2, target, 0);
    int busy_error = errno;
    if (write(release[1], &token, 1) != 1 || waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status)) {
        fprintf(stderr, "child did not complete its exit-fs handoff\n");
        return 1;
    }
    int result = busy_result ? syscall(SYS_umount2, target, 0) : 0;
    int error = errno;
    priority.sched_priority = 0;
    if (syscall(SYS_sched_setscheduler, 0, SCHED_OTHER, &priority)) {
        perror("restore scheduler");
        return 1;
    }
    close(ready[0]);
    close(release[1]);
    if (busy_result != -1 || busy_error != EBUSY) {
        fprintf(stderr, "live child cwd was not busy: private_fs=%d result=%d errno=%d\n",
                private_fs, busy_result, busy_error);
        rmdir(target);
        return 1;
    }
    if (result) {
        fprintf(stderr, "waited child still pins mount: errno=%d\n", error);
        umount2(target, MNT_DETACH);
        rmdir(target);
        return 1;
    }
    if (rmdir(target)) {
        perror("remove mountpoint");
        return 1;
    }
    printf("EXIT_FS_RELEASED_BEFORE_WAIT private_fs=%d\n", private_fs);
    return 0;
}

/* Rolling checksum of the raw backing image. The image is compared across a
 * rejected flush to show the writeback really did not reach the store, and
 * across a successful one to show that it did. */
static int image_checksum(int fd, uint64_t *checksum)
{
    unsigned char buffer[16384];
    uint64_t sum = 1469598103934665603ULL;
    if (lseek(fd, 0, SEEK_SET) < 0)
        return -1;
    for (;;) {
        ssize_t count = read(fd, buffer, sizeof(buffer));
        if (count < 0)
            return -1;
        if (count == 0)
            break;
        for (ssize_t i = 0; i < count; i++)
            sum = (sum ^ buffer[i]) * 1099511628211ULL;
    }
    if (lseek(fd, 0, SEEK_SET) < 0)
        return -1;
    *checksum = sum;
    return 0;
}

static int check_loop_mount_lifetime(void)
{
    char image[] = "/tmp/ltp-loop-image.XXXXXX";
    char target[] = "/tmp/ltp-loop-mount.XXXXXX";
    char alias[] = "/tmp/ltp-loop-alias.XXXXXX";
    char other[] = "/tmp/ltp-loop-other.XXXXXX";
    char path[128];
    char content[6];
    int image_fd = -1, loop_fd = -1, held_fd = -1, path_fd = -1;
    int bound = 0, mounted = 0, alias_mounted = 0, other_mounted = 0, result = 1;
    uint64_t image_before = 0, image_after = 0;
    unsigned write_protect = 1;
    struct loop_info64 info = {0};
    image_fd = mkstemp(image);
    if (image_fd < 0 || ftruncate(image_fd, 64 * 1024 * 1024) ||
        !mkdtemp(target) || !mkdtemp(alias) || !mkdtemp(other))
        goto cleanup;
    loop_fd = open("/dev/loop0", O_RDWR | O_CLOEXEC);
    if (loop_fd < 0)
        goto cleanup;
    if (ioctl(loop_fd, LOOP_GET_STATUS64, &info) != -1 || errno != ENXIO) {
        fprintf(stderr, "loop0 was not released by the preceding fixture\n");
        goto cleanup;
    }
    if (ioctl(loop_fd, LOOP_SET_FD, image_fd))
        goto cleanup;
    bound = 1;
    info.lo_flags = LO_FLAGS_AUTOCLEAR;
    if (ioctl(loop_fd, LOOP_SET_STATUS64, &info))
        goto cleanup;
    path_fd = open("/dev/loop0", O_PATH | O_CLOEXEC);
    if (path_fd < 0)
        goto cleanup;
    close(path_fd);
    path_fd = -1;
    if (ioctl(loop_fd, LOOP_GET_STATUS64, &info) || !(info.lo_flags & LO_FLAGS_AUTOCLEAR))
        goto cleanup;

    pid_t formatter = fork();
    if (formatter < 0)
        goto cleanup;
    if (!formatter) {
        execl("/sbin/mkfs.ext4", "mkfs.ext4", "-q", "-F", "/dev/loop0", (char *)NULL);
        _exit(2);
    }
    int status;
    if (waitpid(formatter, &status, 0) != formatter || !WIFEXITED(status) || WEXITSTATUS(status))
        goto cleanup;
    if (mount("/dev/loop0", target, "ext4", 0, NULL))
        goto cleanup;
    mounted = 1;
    if (mount(target, alias, NULL, MS_BIND, NULL))
        goto cleanup;
    alias_mounted = 1;
    snprintf(path, sizeof(path), "%s/held", target);
    held_fd = open(path, O_CREAT | O_RDWR, 0600);
    if (held_fd < 0 || pwrite(held_fd, "before", 6, 0) != 6 || fsync(held_fd))
        goto cleanup;
    if (ioctl(loop_fd, LOOP_CLR_FD) != -1 || errno != EBUSY) {
        fprintf(stderr, "mounted loop allowed LOOP_CLR_FD\n");
        goto cleanup;
    }
    if (ioctl(loop_fd, LOOP_GET_STATUS64, &info) ||
        mount("/dev/loop0", other, "ext4", 0, NULL) != -1 || errno != EBUSY) {
        fprintf(stderr, "mounted loop allowed a second mount\n");
        goto cleanup;
    }
    /* Rejecting the second mount must leave the first one untouched. */
    if (pwrite(held_fd, "usable", 6, 0) != 6 || fsync(held_fd) ||
        pread(held_fd, content, sizeof(content), 0) != (ssize_t)sizeof(content) ||
        memcmp(content, "usable", sizeof(content))) {
        fprintf(stderr, "the mounted loop stopped working after a rejected second mount\n");
        goto cleanup;
    }
    /* A mounted loop that became write-protected cannot persist its dirty
       cache. BLKFLSBUF reports the EIO a rejected writeback would produce,
       leaves the backing image byte-for-byte untouched, and keeps the cached
       data for the next flush. */
    if (ioctl(loop_fd, BLKROSET, &write_protect) ||
        image_checksum(image_fd, &image_before))
        goto cleanup;
    errno = 0;
    if (ioctl(loop_fd, BLKFLSBUF, 0) != -1 || errno != EIO) {
        fprintf(stderr, "write-protected mounted loop flush was not rejected: errno=%d\n", errno);
        goto cleanup;
    }
    if (image_checksum(image_fd, &image_after) || image_after != image_before) {
        fprintf(stderr, "a rejected flush still wrote to the backing image\n");
        goto cleanup;
    }
    /* Clearing the write protection lets the same call write new dirty cache
       back: only the flush may touch the store, so the image changes exactly
       when the flush succeeds. */
    write_protect = 0;
    if (ioctl(loop_fd, BLKROSET, &write_protect) || pwrite(held_fd, "after", 6, 0) != 6)
        goto cleanup;
    if (ioctl(loop_fd, BLKFLSBUF, 0)) {
        fprintf(stderr, "flush after clearing write protection failed: errno=%d\n", errno);
        goto cleanup;
    }
    if (image_checksum(image_fd, &image_after) || image_after == image_before) {
        fprintf(stderr, "a successful flush did not write the dirty cache back\n");
        goto cleanup;
    }
    puts("LOOP_READONLY_FLUSH_REJECTED_THEN_WRITTEN_BACK");
    close(loop_fd);
    loop_fd = -1;
    if (pwrite(held_fd, "shared", 6, 0) != 6 || fsync(held_fd))
        goto cleanup;
    if (syscall(SYS_umount2, target, MNT_DETACH))
        goto cleanup;
    mounted = 0;
    if (pwrite(held_fd, "detach", 6, 0) != 6 || fsync(held_fd))
        goto cleanup;
    loop_fd = open("/dev/loop0", O_RDWR | O_CLOEXEC);
    if (loop_fd < 0 || ioctl(loop_fd, LOOP_GET_STATUS64, &info))
        goto cleanup;
    if (ioctl(loop_fd, LOOP_SET_FD, image_fd) != -1 || errno != EBUSY) {
        fprintf(stderr, "detached open file allowed loop rebinding\n");
        goto cleanup;
    }
    close(loop_fd);
    loop_fd = -1;
    snprintf(path, sizeof(path), "%s/held", alias);
    path_fd = open(path, O_RDONLY | O_CLOEXEC);
    if (path_fd < 0 || pread(path_fd, content, sizeof(content), 0) != (ssize_t)sizeof(content) ||
        memcmp(content, "detach", sizeof(content)))
        goto cleanup;
    close(path_fd);
    path_fd = -1;
    if (syscall(SYS_umount2, alias, MNT_DETACH))
        goto cleanup;
    alias_mounted = 0;
    if (pread(held_fd, content, sizeof(content), 0) != (ssize_t)sizeof(content) ||
        memcmp(content, "detach", sizeof(content)))
        goto cleanup;
    /* Final mount retirement must write dirty pages, without an explicit fsync. */
    if (pwrite(held_fd, "cached", 6, 0) != 6)
        goto cleanup;
    close(held_fd);
    held_fd = -1;
    loop_fd = open("/dev/loop0", O_RDWR | O_CLOEXEC);
    if (loop_fd < 0 || ioctl(loop_fd, LOOP_GET_STATUS64, &info) != -1 || errno != ENXIO) {
        fprintf(stderr, "last detached file did not release its loop backing\n");
        goto cleanup;
    }
    bound = 0;
    if (ioctl(loop_fd, LOOP_SET_FD, image_fd))
        goto cleanup;
    bound = 1;
    memset(&info, 0, sizeof(info));
    info.lo_flags = LO_FLAGS_AUTOCLEAR;
    if (ioctl(loop_fd, LOOP_SET_STATUS64, &info) ||
        mount("/dev/loop0", target, "ext4", 0, NULL))
        goto cleanup;
    mounted = 1;
    snprintf(path, sizeof(path), "%s/held", target);
    held_fd = open(path, O_RDONLY | O_CLOEXEC);
    if (held_fd < 0 || pread(held_fd, content, sizeof(content), 0) != (ssize_t)sizeof(content) ||
        memcmp(content, "cached", sizeof(content))) {
        fprintf(stderr, "final mount retirement lost dirty data\n");
        goto cleanup;
    }
    close(held_fd);
    held_fd = -1;
    if (syscall(SYS_umount2, target, 0))
        goto cleanup;
    mounted = 0;
    /* A committed unmount releases its mount holder, so the same backing store
       is mountable again and still carries what the first mount wrote. */
    if (mount("/dev/loop0", other, "ext4", 0, NULL))
        goto cleanup;
    other_mounted = 1;
    snprintf(path, sizeof(path), "%s/held", other);
    held_fd = open(path, O_RDONLY | O_CLOEXEC);
    if (held_fd < 0 || pread(held_fd, content, sizeof(content), 0) != (ssize_t)sizeof(content) ||
        memcmp(content, "cached", sizeof(content))) {
        fprintf(stderr, "the same backing was not mountable after an unmount\n");
        goto cleanup;
    }
    close(held_fd);
    held_fd = -1;
    if (syscall(SYS_umount2, other, 0))
        goto cleanup;
    other_mounted = 0;
    puts("LOOP_MOUNT_REUSABLE_AFTER_UNMOUNT");
    puts("LOOP_MOUNT_LEASE_RELEASED_AFTER_LAST_FILE");
    result = 0;
cleanup:
    if (result)
        perror("loop mount lifecycle");
    if (path_fd >= 0)
        close(path_fd);
    if (held_fd >= 0)
        close(held_fd);
    if (alias_mounted)
        umount2(alias, MNT_DETACH);
    if (mounted)
        umount2(target, MNT_DETACH);
    if (other_mounted)
        umount2(other, MNT_DETACH);
    if (bound && loop_fd >= 0)
        ioctl(loop_fd, LOOP_CLR_FD);
    if (loop_fd >= 0)
        close(loop_fd);
    if (image_fd >= 0)
        close(image_fd);
    unlink(image);
    rmdir(target);
    rmdir(alias);
    rmdir(other);
    return result;
}

int main(void)
{
    int normal = check_exit_fs(0);
    int unshared = check_exit_fs(1);
    int loop_mount = check_loop_mount_lifetime();
    return normal || unshared || loop_mount;
}
