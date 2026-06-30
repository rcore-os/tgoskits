/*
 * bug-dir-cookie-unlink-rmdir: getdents64 offsets must remain stable while
 * userland deletes entries from the directory being iterated.
 *
 * Tools such as rm -rf read a batch of directory entries, unlink those names,
 * then continue getdents64 from the returned d_off. Filesystems must therefore
 * return stable directory cookies. Count-based offsets can skip live entries
 * after deletion and make the final rmdir fail with ENOTEMPTY.
 */

#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_getdents64
#if defined(__x86_64__)
#define SYS_getdents64 217
#elif defined(__aarch64__)
#define SYS_getdents64 61
#elif defined(__riscv)
#define SYS_getdents64 61
#elif defined(__loongarch64)
#define SYS_getdents64 61
#endif
#endif

struct linux_dirent64 {
    uint64_t d_ino;
    int64_t d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[];
};

static int fail_errno(const char *phase, const char *path)
{
    printf("FAIL: %s path=%s errno=%d (%s)\n", phase, path, errno, strerror(errno));
    printf("STARRY_GROUPED_TEST_FAILED: bug-dir-cookie-unlink-rmdir\n");
    return 1;
}

static int fail_msg(const char *phase, const char *path, const char *msg)
{
    printf("FAIL: %s path=%s %s\n", phase, path, msg);
    printf("STARRY_GROUPED_TEST_FAILED: bug-dir-cookie-unlink-rmdir\n");
    return 1;
}

static int make_files(const char *dir, const char *prefix, int count)
{
    char path[160];

    for (int i = 0; i < count; i++) {
        snprintf(path, sizeof(path), "%s/%s%04d", dir, prefix, i);
        int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
        if (fd < 0) {
            return fail_errno("create file", path);
        }
        if (write(fd, "x", 1) != 1) {
            close(fd);
            return fail_errno("write file", path);
        }
        close(fd);
    }
    return 0;
}

static int remove_by_batched_getdents(const char *dir)
{
    char buf[80];
    int fd = open(dir, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        return fail_errno("open directory", dir);
    }

    for (;;) {
        int nread = (int)syscall(SYS_getdents64, fd, buf, sizeof(buf));
        if (nread < 0) {
            close(fd);
            return fail_errno("getdents64", dir);
        }
        if (nread == 0) {
            break;
        }

        for (int pos = 0; pos < nread;) {
            struct linux_dirent64 *d = (struct linux_dirent64 *)(void *)(buf + pos);
            if (d->d_reclen == 0 || pos + d->d_reclen > nread) {
                close(fd);
                return fail_msg("parse dirent", dir, "invalid reclen");
            }
            if (strcmp(d->d_name, ".") != 0 && strcmp(d->d_name, "..") != 0) {
                if (unlinkat(fd, d->d_name, 0) != 0) {
                    close(fd);
                    return fail_errno("unlinkat", d->d_name);
                }
            }
            pos += d->d_reclen;
        }
    }

    close(fd);
    return 0;
}

static int cleanup_dir(const char *dir)
{
    char buf[512];
    int fd = open(dir, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        if (errno == ENOENT) {
            return 0;
        }
        if (errno == ENOTDIR && unlink(dir) == 0) {
            return 0;
        }
        return fail_errno("open cleanup dir", dir);
    }

    for (;;) {
        int nread = (int)syscall(SYS_getdents64, fd, buf, sizeof(buf));
        if (nread < 0) {
            close(fd);
            return fail_errno("cleanup getdents64", dir);
        }
        if (nread == 0) {
            break;
        }

        for (int pos = 0; pos < nread;) {
            struct linux_dirent64 *d = (struct linux_dirent64 *)(void *)(buf + pos);
            if (d->d_reclen == 0 || pos + d->d_reclen > nread) {
                close(fd);
                return fail_msg("cleanup parse dirent", dir, "invalid reclen");
            }
            if (strcmp(d->d_name, ".") != 0 && strcmp(d->d_name, "..") != 0) {
                if (unlinkat(fd, d->d_name, 0) != 0) {
                    if ((errno != EISDIR && errno != EPERM) ||
                        unlinkat(fd, d->d_name, AT_REMOVEDIR) != 0) {
                        close(fd);
                        return fail_errno("cleanup unlinkat", d->d_name);
                    }
                }
            }
            pos += d->d_reclen;
        }
    }

    close(fd);
    if (rmdir(dir) != 0 && errno != ENOENT) {
        return fail_errno("cleanup rmdir", dir);
    }
    return 0;
}

static int run_case(const char *label, const char *dir)
{
    const int count = 160;

    printf("[TEST] %s dir=%s count=%d\n", label, dir, count);
    if (cleanup_dir(dir) != 0) {
        return 1;
    }
    if (mkdir(dir, 0755) != 0) {
        return fail_errno("mkdir", dir);
    }
    if (make_files(dir, "f", count) != 0) {
        return 1;
    }
    if (remove_by_batched_getdents(dir) != 0) {
        return 1;
    }
    if (rmdir(dir) != 0) {
        return fail_errno("rmdir after batched unlink", dir);
    }
    printf("PASS: %s batched getdents64 unlink rmdir\n", label);
    return 0;
}

static int run_rename_case(const char *label, const char *src_dir, const char *dst_dir)
{
    const int count = 64;
    char old_path[160];
    char new_path[160];

    printf("[TEST] %s rename dir-cookie src=%s dst=%s count=%d\n", label, src_dir, dst_dir,
           count);
    if (cleanup_dir(src_dir) != 0 || cleanup_dir(dst_dir) != 0) {
        return 1;
    }
    if (mkdir(src_dir, 0755) != 0) {
        return fail_errno("mkdir src", src_dir);
    }
    if (mkdir(dst_dir, 0755) != 0) {
        return fail_errno("mkdir dst", dst_dir);
    }
    if (make_files(src_dir, "s", count) != 0 || make_files(dst_dir, "d", count) != 0) {
        return 1;
    }

    for (int i = 0; i < count; i++) {
        snprintf(old_path, sizeof(old_path), "%s/s%04d", src_dir, i);
        snprintf(new_path, sizeof(new_path), "%s/r%04d", dst_dir, i);
        if (rename(old_path, new_path) != 0) {
            return fail_errno("rename into dst", old_path);
        }
    }

    if (rmdir(src_dir) != 0) {
        return fail_errno("rmdir emptied src", src_dir);
    }
    if (remove_by_batched_getdents(dst_dir) != 0) {
        return 1;
    }
    if (rmdir(dst_dir) != 0) {
        return fail_errno("rmdir dst after renamed batched unlink", dst_dir);
    }
    printf("PASS: %s rename cookie remains unique after batched unlink\n", label);
    return 0;
}

int main(void)
{
    char tmpfs_dir[96];
    char rootfs_dir[96];
    char tmpfs_src[96];
    char tmpfs_dst[96];
    char rootfs_src[96];
    char rootfs_dst[96];
    long pid = (long)getpid();

    printf("=== bug-dir-cookie-unlink-rmdir ===\n");

    snprintf(tmpfs_dir, sizeof(tmpfs_dir), "/tmp/bug_dir_cookie_tmpfs_%ld", pid);
    snprintf(rootfs_dir, sizeof(rootfs_dir), "/root/bug_dir_cookie_ext4_%ld", pid);
    snprintf(tmpfs_src, sizeof(tmpfs_src), "/tmp/bug_dir_cookie_rename_src_%ld", pid);
    snprintf(tmpfs_dst, sizeof(tmpfs_dst), "/tmp/bug_dir_cookie_rename_dst_%ld", pid);
    snprintf(rootfs_src, sizeof(rootfs_src), "/root/bug_dir_cookie_rename_src_%ld", pid);
    snprintf(rootfs_dst, sizeof(rootfs_dst), "/root/bug_dir_cookie_rename_dst_%ld", pid);

    if (run_case("tmpfs", tmpfs_dir) != 0) {
        return 1;
    }
    if (run_case("rootfs", rootfs_dir) != 0) {
        return 1;
    }
    if (run_rename_case("tmpfs", tmpfs_src, tmpfs_dst) != 0) {
        return 1;
    }
    if (run_rename_case("rootfs", rootfs_src, rootfs_dst) != 0) {
        return 1;
    }

    printf("STARRY_GROUPED_TEST_PASSED: bug-dir-cookie-unlink-rmdir\n");
    return 0;
}
