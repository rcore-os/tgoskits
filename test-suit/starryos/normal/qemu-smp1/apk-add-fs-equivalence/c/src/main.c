#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

#define BASE_ROOT "/root"
#define TEST_NAME "apk-add-fs-equivalence"
#define APK_PAYLOAD_CHUNK_SIZE 16384
#define APK_PAYLOAD_CHUNKS 128
#define APK_PAYLOAD_BYTES (APK_PAYLOAD_CHUNK_SIZE * APK_PAYLOAD_CHUNKS)
#define APK_PAYLOAD_BUDGET_MS 30000

static void fail_at(const char *file, int line, const char *message)
{
    printf("APK_ADD_FS_EQUIV_TEST_FAILED: %s:%d: %s (errno=%d %s)\n",
           file, line, message, errno, strerror(errno));
    fflush(stdout);
    exit(1);
}

#define CHECK(cond, message) \
    do { \
        if (!(cond)) { \
            fail_at(__FILE__, __LINE__, (message)); \
        } \
    } while (0)

#define CHECK_CHOWN(call, message) \
    do { \
        if ((call) != 0) { \
            int saved_errno = errno; \
            if (!(saved_errno == EPERM && geteuid() != 0)) { \
                errno = saved_errno; \
                fail_at(__FILE__, __LINE__, (message)); \
            } \
        } \
    } while (0)

static void path_join(char *out, size_t out_len, const char *base, const char *rel)
{
    int n = snprintf(out, out_len, "%s/%s", base, rel);
    CHECK(n > 0 && (size_t)n < out_len, "path too long");
}

static void make_dir(const char *path, mode_t mode)
{
    if (mkdir(path, mode) == 0) {
        return;
    }
    CHECK(errno == EEXIST, "mkdir failed");
}

static void make_child_dir(const char *base, const char *rel, mode_t mode)
{
    char path[PATH_MAX];
    path_join(path, sizeof(path), base, rel);
    make_dir(path, mode);
}

static void write_all(int fd, const void *buf, size_t len)
{
    const char *p = (const char *)buf;
    while (len > 0) {
        ssize_t n = write(fd, p, len);
        CHECK(n > 0, "write failed");
        p += n;
        len -= (size_t)n;
    }
}

static void pwrite_all(int fd, const void *buf, size_t len, off_t offset)
{
    const char *p = (const char *)buf;
    while (len > 0) {
        ssize_t n = pwrite(fd, p, len, offset);
        CHECK(n > 0, "pwrite failed");
        p += n;
        offset += n;
        len -= (size_t)n;
    }
}

static void read_exact_at(int fd, void *buf, size_t len, off_t offset)
{
    char *p = (char *)buf;
    while (len > 0) {
        ssize_t n = pread(fd, p, len, offset);
        CHECK(n > 0, "pread failed");
        p += n;
        offset += n;
        len -= (size_t)n;
    }
}

static long monotonic_ms(void)
{
    struct timespec ts;
    CHECK(clock_gettime(CLOCK_MONOTONIC, &ts) == 0, "clock_gettime failed");
    return ts.tv_sec * 1000L + ts.tv_nsec / 1000000L;
}

static void read_file(const char *path, char *buf, size_t buf_len)
{
    int fd = open(path, O_RDONLY);
    CHECK(fd >= 0, "open for read failed");
    ssize_t n = read(fd, buf, buf_len - 1);
    CHECK(n >= 0, "read failed");
    buf[n] = '\0';
    CHECK(close(fd) == 0, "close after read failed");
}

static void write_file_atomic(const char *tmp_path, const char *final_path,
                              const char *content, mode_t mode)
{
    int fd = open(tmp_path, O_WRONLY | O_CREAT | O_TRUNC, mode);
    CHECK(fd >= 0, "open temp file failed");
    write_all(fd, content, strlen(content));
    CHECK(fchmod(fd, mode) == 0, "fchmod temp file failed");
    CHECK_CHOWN(fchown(fd, geteuid(), getegid()), "fchown temp file failed");
    CHECK(fdatasync(fd) == 0, "fdatasync temp file failed");
    CHECK(fsync(fd) == 0, "fsync temp file failed");
    CHECK(close(fd) == 0, "close temp file failed");
    CHECK(chmod(tmp_path, mode) == 0, "chmod temp file failed");
    CHECK_CHOWN(chown(tmp_path, geteuid(), getegid()), "chown temp file failed");

    struct timespec times[2] = {
        {.tv_sec = 1700000000, .tv_nsec = 0},
        {.tv_sec = 1700000001, .tv_nsec = 0},
    };
    CHECK(utimensat(AT_FDCWD, tmp_path, times, 0) == 0, "utimensat temp file failed");
    CHECK(rename(tmp_path, final_path) == 0, "rename temp to final failed");
    CHECK(access(tmp_path, F_OK) != 0 && errno == ENOENT, "renamed temp file still exists");
}

static void verify_file_content(const char *path, const char *expected)
{
    char buf[512];
    memset(buf, 0, sizeof(buf));
    read_file(path, buf, sizeof(buf));
    CHECK(strcmp(buf, expected) == 0, "file content mismatch");
}

static void rm_rf(const char *path)
{
    struct stat st;
    if (lstat(path, &st) != 0) {
        return;
    }

    if (S_ISDIR(st.st_mode)) {
        DIR *dir = opendir(path);
        if (dir) {
            struct dirent *ent;
            while ((ent = readdir(dir)) != NULL) {
                if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0) {
                    continue;
                }
                char child[PATH_MAX];
                path_join(child, sizeof(child), path, ent->d_name);
                rm_rf(child);
            }
            closedir(dir);
        }
        rmdir(path);
    } else {
        unlink(path);
    }
}

static int count_regular_entries(const char *path)
{
    DIR *dir = opendir(path);
    CHECK(dir != NULL, "opendir failed");

    int count = 0;
    struct dirent *ent;
    while ((ent = readdir(dir)) != NULL) {
        if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0) {
            continue;
        }
        count++;
    }
    CHECK(closedir(dir) == 0, "closedir failed");
    return count;
}

static void create_package_tree(const char *base)
{
    make_dir(base, 0755);
    int basefd = open(base, O_RDONLY | O_DIRECTORY);
    CHECK(basefd >= 0, "open base dir failed");
    CHECK(mkdirat(basefd, "usr", 0755) == 0, "mkdirat usr failed");
    CHECK(fstatat(basefd, "usr", &(struct stat){0}, 0) == 0, "fstatat usr failed");
    CHECK(close(basefd) == 0, "close base dir failed");

    make_child_dir(base, "usr/bin", 0755);
    make_child_dir(base, "usr/lib", 0755);
    make_child_dir(base, "usr/share", 0755);
    make_child_dir(base, "usr/share/doc", 0755);
    make_child_dir(base, "usr/share/doc/pkg", 0755);
    make_child_dir(base, "lib", 0755);
    make_child_dir(base, "lib/apk", 0755);
    make_child_dir(base, "lib/apk/db", 0755);
    make_child_dir(base, "var", 0755);
    make_child_dir(base, "var/cache", 0755);
    make_child_dir(base, "var/cache/apk", 0755);
    make_child_dir(base, "var/lib", 0755);
    make_child_dir(base, "var/lib/dpkg", 0755);
    make_child_dir(base, "var/lib/dpkg/info", 0755);
    make_child_dir(base, "etc", 0755);
    make_child_dir(base, "etc/apk", 0755);
}

static void test_lock_file(const char *base)
{
    char lock[PATH_MAX];
    path_join(lock, sizeof(lock), base, "lib/apk/db/lock");

    int fd = open(lock, O_RDWR | O_CREAT | O_EXCL, 0600);
    CHECK(fd >= 0, "create exclusive lock file failed");
    write_all(fd, "locked\n", 7);
    CHECK(fsync(fd) == 0, "fsync lock failed");

    int second = open(lock, O_RDWR | O_CREAT | O_EXCL, 0600);
    CHECK(second < 0 && errno == EEXIST, "O_EXCL lock did not reject existing file");
    CHECK(close(fd) == 0, "close lock failed");
}

static void test_payload_replace(const char *base)
{
    char tmp[PATH_MAX], final[PATH_MAX], link_path[PATH_MAX], hard_path[PATH_MAX];
    path_join(tmp, sizeof(tmp), base, "usr/bin/pkg-tool.apk-new");
    path_join(final, sizeof(final), base, "usr/bin/pkg-tool");
    path_join(link_path, sizeof(link_path), base, "usr/bin/pkg-tool-link");
    path_join(hard_path, sizeof(hard_path), base, "usr/bin/pkg-tool-hardlink");

    write_file_atomic(tmp, final, "#!/bin/sh\necho package-fs\n", 0755);
    verify_file_content(final, "#!/bin/sh\necho package-fs\n");

    struct stat st;
    CHECK(stat(final, &st) == 0, "stat final payload failed");
    CHECK((st.st_mode & 0777) == 0755, "payload mode mismatch");
    CHECK(st.st_uid == geteuid() && st.st_gid == getegid(), "payload owner mismatch");
    CHECK(st.st_mtime == 1700000001, "payload mtime mismatch");

    CHECK(symlink("pkg-tool", link_path) == 0, "symlink failed");
    char target[64];
    ssize_t n = readlink(link_path, target, sizeof(target) - 1);
    CHECK(n > 0, "readlink failed");
    target[n] = '\0';
    CHECK(strcmp(target, "pkg-tool") == 0, "symlink target mismatch");
    CHECK(lstat(link_path, &st) == 0 && S_ISLNK(st.st_mode), "lstat symlink failed");
    CHECK_CHOWN(lchown(link_path, geteuid(), getegid()), "lchown symlink failed");

    CHECK(link(final, hard_path) == 0, "hard link failed");
    CHECK(stat(hard_path, &st) == 0, "stat hard link failed");
    CHECK(st.st_nlink >= 2, "hard link count mismatch");
    verify_file_content(hard_path, "#!/bin/sh\necho package-fs\n");
}

static void test_database_rewrite(const char *base)
{
    char tmp[PATH_MAX], installed[PATH_MAX], status[PATH_MAX], status_tmp[PATH_MAX];
    path_join(tmp, sizeof(tmp), base, "lib/apk/db/installed.new");
    path_join(installed, sizeof(installed), base, "lib/apk/db/installed");
    path_join(status, sizeof(status), base, "var/lib/dpkg/status");
    path_join(status_tmp, sizeof(status_tmp), base, "var/lib/dpkg/status.dpkg-new");

    write_file_atomic(tmp, installed,
                      "P:pkg-tool\nV:1.0-r0\nF:usr/bin/pkg-tool\n", 0644);
    verify_file_content(installed, "P:pkg-tool\nV:1.0-r0\nF:usr/bin/pkg-tool\n");

    write_file_atomic(tmp, installed,
                      "P:pkg-tool\nV:1.1-r0\nF:usr/bin/pkg-tool\n", 0644);
    verify_file_content(installed, "P:pkg-tool\nV:1.1-r0\nF:usr/bin/pkg-tool\n");

    int fd = open(status, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0, "open dpkg status failed");
    const char *tool_status = "Package: pkg-tool\nStatus: install ok installed\n\n";
    write_all(fd, tool_status, strlen(tool_status));
    CHECK(close(fd) == 0, "close dpkg status failed");

    fd = open(status, O_WRONLY | O_APPEND);
    CHECK(fd >= 0, "open dpkg status append failed");
    const char *lib_status = "Package: pkg-lib\nStatus: install ok installed\n\n";
    write_all(fd, lib_status, strlen(lib_status));
    CHECK(close(fd) == 0, "close dpkg status append failed");

    CHECK(truncate(status, (off_t)strlen(tool_status)) == 0, "truncate dpkg status failed");
    verify_file_content(status, tool_status);

    write_file_atomic(status_tmp, status,
                      "Package: pkg-tool\nStatus: install ok unpacked\n\n", 0644);
    verify_file_content(status, "Package: pkg-tool\nStatus: install ok unpacked\n\n");
}

static void test_sparse_library_io(const char *base)
{
    char path[PATH_MAX];
    path_join(path, sizeof(path), base, "usr/lib/libpkgpayload.so");

    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0, "open library payload failed");
    const char *head = "ELF-package-head";
    const char *tail = "ELF-package-tail";
    pwrite_all(fd, head, strlen(head), 0);
    pwrite_all(fd, tail, strlen(tail), 4096);
    CHECK(ftruncate(fd, 8192) == 0, "ftruncate grow failed");
    CHECK(fdatasync(fd) == 0, "fdatasync library failed");
    CHECK(fsync(fd) == 0, "fsync library failed");

    char buf[64];
    memset(buf, 0, sizeof(buf));
    read_exact_at(fd, buf, strlen(head), 0);
    CHECK(strcmp(buf, head) == 0, "pread head mismatch");
    memset(buf, 0, sizeof(buf));
    read_exact_at(fd, buf, strlen(tail), 4096);
    CHECK(strcmp(buf, tail) == 0, "pread tail mismatch");

    CHECK(ftruncate(fd, 64) == 0, "ftruncate shrink failed");
    struct stat st;
    CHECK(fstat(fd, &st) == 0, "fstat library failed");
    CHECK(st.st_size == 64, "library shrink size mismatch");
    CHECK(close(fd) == 0, "close library failed");
}

static void test_many_small_files(const char *base)
{
    char dir[PATH_MAX];
    path_join(dir, sizeof(dir), base, "usr/share/doc/pkg");

    for (int i = 0; i < 64; i++) {
        char path[PATH_MAX];
        char data[128];
        int n = snprintf(path, sizeof(path), "%s/file-%02d.txt", dir, i);
        CHECK(n > 0 && (size_t)n < sizeof(path), "small file path too long");
        n = snprintf(data, sizeof(data), "payload-file-%02d\n", i);
        CHECK(n > 0 && (size_t)n < sizeof(data), "small file data too long");

        int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
        CHECK(fd >= 0, "open small file failed");
        write_all(fd, data, strlen(data));
        if (i % 8 == 0) {
            CHECK(fdatasync(fd) == 0, "fdatasync small file failed");
        }
        CHECK(close(fd) == 0, "close small file failed");

        char readback[128];
        memset(readback, 0, sizeof(readback));
        read_file(path, readback, sizeof(readback));
        CHECK(strcmp(readback, data) == 0, "small file content mismatch");
    }

    CHECK(count_regular_entries(dir) == 64, "readdir small file count mismatch");
}

static unsigned char payload_byte(size_t chunk, size_t offset)
{
    return (unsigned char)((chunk * 31 + offset * 17 + 0x5a) & 0xff);
}

static void fill_payload_chunk(unsigned char *buf, size_t chunk)
{
    for (size_t i = 0; i < APK_PAYLOAD_CHUNK_SIZE; i++) {
        buf[i] = payload_byte(chunk, i);
    }
}

static void verify_payload_chunk(const unsigned char *buf, size_t chunk)
{
    for (size_t i = 0; i < APK_PAYLOAD_CHUNK_SIZE; i++) {
        CHECK(buf[i] == payload_byte(chunk, i), "large payload content mismatch");
    }
}

static unsigned long long payload_checksum_update(unsigned long long checksum,
                                                  const unsigned char *buf,
                                                  size_t len)
{
    for (size_t i = 0; i < len; i++) {
        checksum = (checksum * 1315423911ULL) ^ (unsigned long long)buf[i] ^ i;
    }
    return checksum;
}

static void test_large_apk_payload_io(const char *base)
{
    char tmp[PATH_MAX], final[PATH_MAX];
    unsigned char buf[APK_PAYLOAD_CHUNK_SIZE];
    size_t written = 0;
    size_t readback = 0;
    unsigned long long write_checksum = 0;
    unsigned long long read_checksum = 0;

    path_join(tmp, sizeof(tmp), base, "var/cache/apk/large-package.apk-new");
    path_join(final, sizeof(final), base, "usr/lib/large-package.dat");

    long start_ms = monotonic_ms();
    int fd = open(tmp, O_RDWR | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0, "open large apk payload failed");

    for (size_t chunk = 0; chunk < APK_PAYLOAD_CHUNKS; chunk++) {
        fill_payload_chunk(buf, chunk);
        write_checksum = payload_checksum_update(write_checksum, buf, sizeof(buf));
        write_all(fd, buf, sizeof(buf));
        written += sizeof(buf);
    }
    CHECK(fdatasync(fd) == 0, "fdatasync large apk payload failed");
    CHECK(fsync(fd) == 0, "fsync large apk payload failed");
    CHECK(close(fd) == 0, "close large apk payload failed");
    CHECK(rename(tmp, final) == 0, "rename large apk payload failed");

    fd = open(final, O_RDONLY);
    CHECK(fd >= 0, "open large apk payload readback failed");
    for (size_t chunk = 0; chunk < APK_PAYLOAD_CHUNKS; chunk++) {
        read_exact_at(fd, buf, sizeof(buf), (off_t)(chunk * APK_PAYLOAD_CHUNK_SIZE));
        verify_payload_chunk(buf, chunk);
        read_checksum = payload_checksum_update(read_checksum, buf, sizeof(buf));
        readback += sizeof(buf);
    }
    CHECK(close(fd) == 0, "close large apk payload readback failed");
    CHECK(written == APK_PAYLOAD_BYTES, "large payload written byte count mismatch");
    CHECK(readback == APK_PAYLOAD_BYTES, "large payload readback byte count mismatch");
    CHECK(readback == written, "large payload readback length differs from written length");
    CHECK(read_checksum == write_checksum, "large payload checksum mismatch");

    long elapsed_ms = monotonic_ms() - start_ms;
    printf("APK_ADD_FS_EQUIV_LARGE_PAYLOAD_WRITE_BYTES=%zu CHECKSUM=%llu\n",
           written, write_checksum);
    printf("APK_ADD_FS_EQUIV_LARGE_PAYLOAD_READ_BYTES=%zu CHECKSUM=%llu\n",
           readback, read_checksum);
    printf("APK_ADD_FS_EQUIV_LARGE_PAYLOAD_MS=%ld\n", elapsed_ms);
    CHECK(elapsed_ms < APK_PAYLOAD_BUDGET_MS, "large apk payload I/O exceeded budget");
}

static void test_cross_directory_rename_and_cleanup(const char *base)
{
    char tmp[PATH_MAX], final[PATH_MAX], empty_dir[PATH_MAX], apk_db[PATH_MAX];
    path_join(tmp, sizeof(tmp), base, "var/cache/apk/pkg-tool.meta.tmp");
    path_join(final, sizeof(final), base, "usr/lib/pkg-tool.meta");
    path_join(empty_dir, sizeof(empty_dir), base, "var/cache/apk/empty");
    path_join(apk_db, sizeof(apk_db), base, "lib/apk/db");

    int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0, "open cross-dir temp failed");
    write_all(fd, "cached package metadata\n", 24);
    CHECK(fsync(fd) == 0, "fsync cross-dir temp failed");
    CHECK(close(fd) == 0, "close cross-dir temp failed");
    CHECK(rename(tmp, final) == 0, "cross-directory rename failed");
    verify_file_content(final, "cached package metadata\n");

    make_dir(empty_dir, 0755);
    CHECK(rmdir(empty_dir) == 0, "rmdir empty cache dir failed");
    CHECK(unlink(final) == 0, "unlink renamed metadata failed");

    int dirfd = open(apk_db, O_RDONLY | O_DIRECTORY);
    CHECK(dirfd >= 0, "open apk db dir failed");
    CHECK(fsync(dirfd) == 0, "fsync apk db dir failed");
    CHECK(fdatasync(dirfd) == 0, "fdatasync apk db dir failed");
    CHECK(syncfs(dirfd) == 0, "syncfs apk db dir failed");
    CHECK(close(dirfd) == 0, "close apk db dir failed");
    sync();
}

int main(void)
{
    const char *base_root = getenv("STARRY_APK_ADD_FS_BASE");
    if (base_root == NULL || base_root[0] == '\0') {
        base_root = BASE_ROOT;
    }
    const char *guest_paths[] = {
        "/usr/bin",
        "/usr/lib",
        "/lib/apk/db",
        "/var/lib/dpkg",
    };
    for (size_t i = 0; i < sizeof(guest_paths) / sizeof(guest_paths[0]); i++) {
        printf("simulate package path: %s\n", guest_paths[i]);
    }

    char base[PATH_MAX];
    int n = snprintf(base, sizeof(base), "%s/%s-%ld", base_root, TEST_NAME, (long)getpid());
    CHECK(n > 0 && (size_t)n < sizeof(base), "base path too long");

    rm_rf(base);
    create_package_tree(base);
    test_lock_file(base);
    test_payload_replace(base);
    test_database_rewrite(base);
    test_sparse_library_io(base);
    test_many_small_files(base);
    test_large_apk_payload_io(base);
    test_cross_directory_rename_and_cleanup(base);
    rm_rf(base);

    printf("APK_ADD_FS_EQUIV_TEST_PASSED\n");
    fflush(stdout);
    return 0;
}
