#define _GNU_SOURCE
#include "helpers.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

void cleanup_tree(const char *path)
{
    DIR *d = opendir(path);
    if (d) {
        struct dirent *e;
        while ((e = readdir(d)) != NULL) {
            if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, ".."))
                continue;
            char child[1024];
            snprintf(child, sizeof(child), "%s/%s", path, e->d_name);
            struct stat st;
            if (lstat(child, &st) == 0) {
                if (S_ISDIR(st.st_mode))
                    cleanup_tree(child);
                else
                    unlink(child);
            }
        }
        closedir(d);
    }
    rmdir(path);
    /* 顶层若是 symlink/普通文件，rmdir 不掉，再 unlink 兜底 */
    unlink(path);
}

int ensure_dir(const char *path)
{
    if (mkdir(path, 0755) == 0) return 0;
    if (errno == EEXIST) {
        struct stat st;
        if (stat(path, &st) == 0 && S_ISDIR(st.st_mode)) return 0;
    }
    return -1;
}

int write_file(const char *path, const void *content, size_t len, mode_t mode)
{
    unlink(path);
    int fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, mode);
    if (fd < 0) return -1;
    ssize_t off = 0;
    while ((size_t)off < len) {
        ssize_t n = write(fd, (const char *)content + off, len - off);
        if (n <= 0) { close(fd); return -1; }
        off += n;
    }
    close(fd);
    /* O_CREAT mode 受 umask 影响，强制再 chmod 到目标 mode 以便测试 */
    if (chmod(path, mode) != 0) return -1;
    return 0;
}

ssize_t read_file(const char *path, char *buf, size_t buf_size)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    ssize_t total = 0;
    while ((size_t)total + 1 < buf_size) {
        ssize_t n = read(fd, buf + total, buf_size - 1 - total);
        if (n < 0) { close(fd); return -1; }
        if (n == 0) break;
        total += n;
    }
    buf[total] = '\0';
    close(fd);
    return total;
}

mode_t get_file_mode(const char *path)
{
    struct stat st;
    if (stat(path, &st) != 0) return (mode_t)-1;
    return st.st_mode & 07777;
}

off_t get_file_size(const char *path)
{
    struct stat st;
    if (stat(path, &st) != 0) return (off_t)-1;
    return st.st_size;
}

int is_regular_file(const char *path)
{
    struct stat st;
    return stat(path, &st) == 0 && S_ISREG(st.st_mode);
}

int is_directory(const char *path)
{
    struct stat st;
    return stat(path, &st) == 0 && S_ISDIR(st.st_mode);
}
