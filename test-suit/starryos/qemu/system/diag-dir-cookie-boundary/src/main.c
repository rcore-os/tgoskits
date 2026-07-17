#define main original_bug_dir_cookie_main
#include "../../bugfix-bug-dir-cookie-unlink-rmdir/src/main.c"
#undef main

#include <time.h>

static long long monotonic_us(void)
{
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
        return -1;
    }
    return (long long)now.tv_sec * 1000000LL + now.tv_nsec / 1000;
}

static int make_files_with_trace(const char *dir, const char *prefix, int count)
{
    char path[160];

    for (int i = 0; i < count; i++) {
        snprintf(path, sizeof(path), "%s/%s%04d", dir, prefix, i);
        printf("DIAG t_us=%lld phase=create-open-before index=%d path=%s\n", monotonic_us(), i,
               path);
        int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
        printf("DIAG t_us=%lld phase=create-open-after index=%d fd=%d errno=%d\n",
               monotonic_us(), i, fd, errno);
        if (fd < 0) {
            return fail_errno("diag create file", path);
        }
        printf("DIAG t_us=%lld phase=create-write-before index=%d\n", monotonic_us(), i);
        ssize_t written = write(fd, "x", 1);
        printf("DIAG t_us=%lld phase=create-write-after index=%d result=%lld errno=%d\n",
               monotonic_us(), i, (long long)written, errno);
        if (written != 1) {
            close(fd);
            return fail_errno("diag write file", path);
        }
        printf("DIAG t_us=%lld phase=create-close-before index=%d\n", monotonic_us(), i);
        int result = close(fd);
        printf("DIAG t_us=%lld phase=create-close-after index=%d result=%d errno=%d\n",
               monotonic_us(), i, result, errno);
        if (result != 0) {
            return fail_errno("diag close file", path);
        }
    }
    return 0;
}

static int remove_with_trace(const char *dir)
{
    char buf[80];
    int fd = open(dir, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        return fail_errno("diag open directory", dir);
    }

    for (int batch = 0;; batch++) {
        printf("DIAG t_us=%lld phase=getdents-before batch=%d\n", monotonic_us(), batch);
        int nread = (int)syscall(SYS_getdents64, fd, buf, sizeof(buf));
        printf("DIAG t_us=%lld phase=getdents-after batch=%d nread=%d errno=%d\n",
               monotonic_us(), batch, nread, errno);
        if (nread < 0) {
            close(fd);
            return fail_errno("diag getdents64", dir);
        }
        if (nread == 0) {
            break;
        }

        for (int pos = 0; pos < nread;) {
            struct linux_dirent64 *entry = (struct linux_dirent64 *)(void *)(buf + pos);
            if (entry->d_reclen == 0 || pos + entry->d_reclen > nread) {
                close(fd);
                return fail_msg("diag parse dirent", dir, "invalid reclen");
            }
            if (strcmp(entry->d_name, ".") != 0 && strcmp(entry->d_name, "..") != 0) {
                printf("DIAG t_us=%lld phase=unlink-before batch=%d name=%s cookie=%lld\n",
                       monotonic_us(), batch, entry->d_name, (long long)entry->d_off);
                int result = unlinkat(fd, entry->d_name, 0);
                printf(
                    "DIAG t_us=%lld phase=unlink-after batch=%d name=%s result=%d errno=%d\n",
                    monotonic_us(), batch, entry->d_name, result, errno);
                if (result != 0) {
                    close(fd);
                    return fail_errno("diag unlinkat", entry->d_name);
                }
            }
            pos += entry->d_reclen;
        }
    }

    close(fd);
    return 0;
}

int main(void)
{
    char src_dir[96];
    char dst_dir[96];
    char old_path[160];
    char new_path[160];
    long pid = (long)getpid();
    const int count = 64;

    setvbuf(stdout, NULL, _IONBF, 0);
    snprintf(src_dir, sizeof(src_dir), "/root/diag_cookie_src_%ld", pid);
    snprintf(dst_dir, sizeof(dst_dir), "/root/diag_cookie_dst_%ld", pid);

    printf("DIAG phase=cleanup-before\n");
    if (cleanup_dir(src_dir) != 0 || cleanup_dir(dst_dir) != 0) {
        return 1;
    }
    printf("DIAG phase=cleanup-after\n");
    if (mkdir(src_dir, 0755) != 0 || mkdir(dst_dir, 0755) != 0) {
        return fail_errno("diag mkdir", src_dir);
    }
    printf("DIAG phase=make-src-before\n");
    if (make_files_with_trace(src_dir, "s", count) != 0) {
        return 1;
    }
    printf("DIAG phase=make-src-after\n");
    if (make_files_with_trace(dst_dir, "d", count) != 0) {
        return 1;
    }
    printf("DIAG phase=make-dst-after\n");

    for (int i = 0; i < count; i++) {
        snprintf(old_path, sizeof(old_path), "%s/s%04d", src_dir, i);
        snprintf(new_path, sizeof(new_path), "%s/r%04d", dst_dir, i);
        printf("DIAG t_us=%lld phase=rename-before index=%d\n", monotonic_us(), i);
        if (rename(old_path, new_path) != 0) {
            return fail_errno("diag rename", old_path);
        }
        printf("DIAG t_us=%lld phase=rename-after index=%d\n", monotonic_us(), i);
    }

    printf("DIAG phase=rmdir-src-before\n");
    if (rmdir(src_dir) != 0) {
        return fail_errno("diag rmdir src", src_dir);
    }
    printf("DIAG phase=rmdir-src-after\n");
    if (remove_with_trace(dst_dir) != 0) {
        return 1;
    }
    printf("DIAG phase=rmdir-dst-before\n");
    if (rmdir(dst_dir) != 0) {
        return fail_errno("diag rmdir dst", dst_dir);
    }
    printf("DIAG_DIR_COOKIE_CASE_DONE\n");
    return 0;
}
