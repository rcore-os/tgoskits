#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define CHECK(cond, fmt, ...)                                                    \
    do {                                                                         \
        if (!(cond)) {                                                           \
            fprintf(stderr, "FAIL: " fmt "\n", ##__VA_ARGS__);                 \
            return 1;                                                            \
        }                                                                        \
        printf("PASS: " fmt "\n", ##__VA_ARGS__);                              \
    } while (0)

static int write_all(const char *path, const char *data) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return -1;
    size_t len = strlen(data);
    ssize_t n = write(fd, data, len);
    int saved = errno;
    close(fd);
    errno = saved;
    return n == (ssize_t)len ? 0 : -1;
}

static int read_file(const char *path, char *buf, size_t cap) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    ssize_t n = read(fd, buf, cap - 1);
    int saved = errno;
    close(fd);
    if (n < 0) {
        errno = saved;
        return -1;
    }
    buf[n] = '\0';
    return 0;
}

int main(void) {
    const char *base = "/tmp/bug_rename_file_into_child_dir";
    const char *src = "/tmp/bug_rename_file_into_child_dir/src";
    const char *child = "/tmp/bug_rename_file_into_child_dir/child";
    const char *dst = "/tmp/bug_rename_file_into_child_dir/child/dst";
    const char *dir_src = "/tmp/bug_rename_file_into_child_dir/dir_src";
    const char *dir_child = "/tmp/bug_rename_file_into_child_dir/dir_src/child";
    const char *dir_dst = "/tmp/bug_rename_file_into_child_dir/dir_src/child/moved";
    char buf[64];

    printf("=== bug-rename-file-into-child-dir ===\n");

    unlink(dst);
    unlink(src);
    rmdir(dir_dst);
    rmdir(dir_child);
    rmdir(dir_src);
    rmdir(child);
    rmdir(base);

    CHECK(mkdir(base, 0755) == 0, "mkdir base");
    CHECK(mkdir(child, 0755) == 0, "mkdir child");
    CHECK(write_all(src, "redis-aof-rename") == 0, "create source file");

    CHECK(rename(src, dst) == 0, "rename regular file from parent dir into child dir");
    CHECK(access(src, F_OK) == -1 && errno == ENOENT, "old file path disappeared");
    CHECK(read_file(dst, buf, sizeof(buf)) == 0, "renamed file can be opened");
    CHECK(strcmp(buf, "redis-aof-rename") == 0, "renamed file content is preserved");

    CHECK(mkdir(dir_src, 0755) == 0, "mkdir dir_src");
    CHECK(mkdir(dir_child, 0755) == 0, "mkdir dir_src/child");
    errno = 0;
    CHECK(rename(dir_src, dir_dst) == -1 && errno == EINVAL,
          "rename directory into its descendant still returns EINVAL");

    printf("bug-rename-file-into-child-dir: OK\n");
    return 0;
}
