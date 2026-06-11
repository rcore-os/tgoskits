#define _GNU_SOURCE

#include "test_framework.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

static char base[PATH_MAX];
static char lower[PATH_MAX];
static char upper[PATH_MAX];
static char work[PATH_MAX];
static char merged[PATH_MAX];

static int make_path(char *out, size_t out_len, const char *dir,
                     const char *name)
{
    int ret = snprintf(out, out_len, "%s/%s", dir, name);
    return ret > 0 && (size_t)ret < out_len;
}

static void write_file(const char *path, const char *data)
{
    int fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(fd >= 0, "open file for writing");
    if (fd < 0) {
        return;
    }

    size_t len = strlen(data);
    ssize_t written = write(fd, data, len);
    CHECK(written == (ssize_t)len, "write complete file contents");
    CHECK(close(fd) == 0, "close written file");
}

static void read_file(const char *path, char *buf, size_t buf_len)
{
    int fd = open(path, O_RDONLY);
    CHECK(fd >= 0, "open file for reading");
    if (fd < 0) {
        if (buf_len > 0) {
            buf[0] = '\0';
        }
        return;
    }

    ssize_t nread = read(fd, buf, buf_len - 1);
    CHECK(nread >= 0, "read file contents");
    if (nread >= 0) {
        buf[nread] = '\0';
    } else if (buf_len > 0) {
        buf[0] = '\0';
    }
    CHECK(close(fd) == 0, "close read file");
}

static int dir_contains(const char *dir, const char *name)
{
    DIR *stream = opendir(dir);
    CHECK(stream != NULL, "open directory for scanning");
    if (stream == NULL) {
        return 0;
    }

    int found = 0;
    errno = 0;
    for (;;) {
        struct dirent *entry = readdir(stream);
        if (entry == NULL) {
            break;
        }
        if (strcmp(entry->d_name, name) == 0) {
            found = 1;
            break;
        }
    }
    CHECK(errno == 0, "scan directory without readdir error");
    CHECK(closedir(stream) == 0, "close scanned directory");
    return found;
}

static void mkdir_checked(const char *path)
{
    CHECK(mkdir(path, 0755) == 0, "mkdir");
}

static void setup_paths(void)
{
    int ret = snprintf(base, sizeof(base), "/tmp/overlayfs-test-%ld",
                       (long)getpid());
    CHECK(ret > 0 && (size_t)ret < sizeof(base), "build base path");
    CHECK(make_path(lower, sizeof(lower), base, "lower"), "build lower path");
    CHECK(make_path(upper, sizeof(upper), base, "upper"), "build upper path");
    CHECK(make_path(work, sizeof(work), base, "work"), "build work path");
    CHECK(make_path(merged, sizeof(merged), base, "merged"),
          "build merged path");
}

static void setup_tree(void)
{
    char path[PATH_MAX];
    char path2[PATH_MAX];

    mkdir_checked(base);
    mkdir_checked(lower);
    mkdir_checked(upper);
    mkdir_checked(work);
    mkdir_checked(merged);

    CHECK(make_path(path, sizeof(path), lower, "delete_me"),
          "build lower delete path");
    write_file(path, "lower delete\n");
    CHECK(make_path(path, sizeof(path), lower, "write_me"),
          "build lower write path");
    write_file(path, "lower original\n");
    CHECK(make_path(path, sizeof(path), lower, "lower_only"),
          "build lower-only path");
    write_file(path, "lower visible\n");
    CHECK(make_path(path, sizeof(path), upper, "upper_only"),
          "build upper-only path");
    write_file(path, "upper visible\n");

    CHECK(make_path(path, sizeof(path), lower, "opaque"), "build lower opaque");
    mkdir_checked(path);
    CHECK(make_path(path2, sizeof(path2), path, "lower_child"),
          "build opaque lower child");
    write_file(path2, "hidden\n");

    CHECK(make_path(path, sizeof(path), upper, "opaque"), "build upper opaque");
    mkdir_checked(path);
    CHECK(make_path(path2, sizeof(path2), path, ".wh..wh..opq"),
          "build opaque marker");
    write_file(path2, "");
    CHECK(make_path(path2, sizeof(path2), path, "upper_child"),
          "build opaque upper child");
    write_file(path2, "visible\n");
}

static void mount_overlay(void)
{
    char opts[PATH_MAX * 3];
    int ret = snprintf(opts, sizeof(opts), "lowerdir=%s,upperdir=%s,workdir=%s",
                       lower, upper, work);
    CHECK(ret > 0 && (size_t)ret < sizeof(opts), "build overlay mount data");
    CHECK(mount("overlay", merged, "overlay", 0, opts) == 0,
          "mount overlay filesystem");
}

static void test_read_dir_merge(void)
{
    CHECK(dir_contains(merged, "lower_only"), "read_dir exposes lower entry");
    CHECK(dir_contains(merged, "upper_only"), "read_dir exposes upper entry");
}

static void test_copy_up_write(void)
{
    char path[PATH_MAX];
    char upper_path[PATH_MAX];
    char lower_path[PATH_MAX];
    char buf[64];

    CHECK(make_path(path, sizeof(path), merged, "write_me"),
          "build merged write path");
    int fd = open(path, O_WRONLY | O_TRUNC);
    CHECK(fd >= 0, "open lower-backed file for write");
    if (fd >= 0) {
        const char *data = "upper copy\n";
        CHECK(write(fd, data, strlen(data)) == (ssize_t)strlen(data),
              "write through overlay");
        CHECK(close(fd) == 0, "close overlay write fd");
    }

    CHECK(make_path(upper_path, sizeof(upper_path), upper, "write_me"),
          "build copied-up upper path");
    CHECK(make_path(lower_path, sizeof(lower_path), lower, "write_me"),
          "build original lower path");
    read_file(upper_path, buf, sizeof(buf));
    CHECK(strcmp(buf, "upper copy\n") == 0, "copy-up writes upper file");
    read_file(lower_path, buf, sizeof(buf));
    CHECK(strcmp(buf, "lower original\n") == 0, "copy-up preserves lower file");
}

static void test_whiteout_lookup(void)
{
    char path[PATH_MAX];
    struct stat st;

    CHECK(make_path(path, sizeof(path), merged, "delete_me"),
          "build merged delete path");
    CHECK(unlink(path) == 0, "unlink lower-backed file creates whiteout");
    CHECK(stat(path, &st) == -1 && errno == ENOENT,
          "whiteout hides lower entry from stat lookup");
    CHECK(open(path, O_RDONLY) == -1 && errno == ENOENT,
          "whiteout hides lower entry from open lookup");
    CHECK(!dir_contains(merged, "delete_me"),
          "whiteout hides lower entry from read_dir");
}

static void test_opaque_lookup(void)
{
    char path[PATH_MAX];
    struct stat st;

    CHECK(make_path(path, sizeof(path), merged, "opaque/lower_child"),
          "build opaque lower child path");
    CHECK(stat(path, &st) == -1 && errno == ENOENT,
          "opaque upper directory blocks lower-only lookup");
    CHECK(make_path(path, sizeof(path), merged, "opaque/upper_child"),
          "build opaque upper child path");
    CHECK(stat(path, &st) == 0, "opaque upper directory keeps upper child");

    CHECK(make_path(path, sizeof(path), merged, "opaque"),
          "build opaque directory path");
    CHECK(!dir_contains(path, "lower_child"),
          "opaque upper directory blocks lower-only read_dir entry");
    CHECK(dir_contains(path, "upper_child"),
          "opaque upper directory exposes upper read_dir entry");
}

int main(void)
{
    TEST_START("overlayfs whiteout opaque copy-up lookup semantics");

    setup_paths();
    setup_tree();
    mount_overlay();
    test_read_dir_merge();
    test_copy_up_write();
    test_whiteout_lookup();
    test_opaque_lookup();
    CHECK(umount(merged) == 0, "unmount overlay filesystem");

    TEST_DONE();
}
