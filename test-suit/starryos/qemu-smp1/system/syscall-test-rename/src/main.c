#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <sys/stat.h>
#include <sys/types.h>

#define CHECK(cond, msg) do { \
    if (!(cond)) { printf("FAIL: %s (line %d)\n", (msg), __LINE__); return 1; } \
    else { printf("PASS: %s\n", (msg)); } \
} while(0)

static int exists(const char *path) {
    struct stat st;
    return stat(path, &st) == 0;
}

static int write_file(const char *path, const char *content) {
    FILE *f = fopen(path, "w");
    if (!f) return -1;
    fprintf(f, "%s", content);
    fclose(f);
    return 0;
}

static int read_verify(const char *path, const char *expected) {
    char buf[256];
    FILE *f = fopen(path, "r");
    if (!f) return 0;
    char *r = fgets(buf, sizeof(buf), f);
    fclose(f);
    if (!r) return 0;
    return strcmp(buf, expected) == 0;
}

static int mkdir_p(const char *path) {
    char tmp[256];
    snprintf(tmp, sizeof(tmp), "%s", path);
    for (char *p = tmp + 1; *p; p++) {
        if (*p == '/') {
            *p = 0;
            mkdir(tmp, 0755);
            *p = '/';
        }
    }
    return mkdir(tmp, 0755);
}

static void cleanup() {
    system("rm -rf /tmp/test-rename");
}

int main() {
    const char *base = "/tmp/test-rename";

    cleanup();
    mkdir_p(base);

    /* Test 1: basic rename within same directory */
    CHECK(write_file("/tmp/test-rename/file1.txt", "hello") == 0, "write file1");
    CHECK(rename("/tmp/test-rename/file1.txt", "/tmp/test-rename/file2.txt") == 0, "rename within same dir");
    CHECK(!exists("/tmp/test-rename/file1.txt"), "old name gone");
    CHECK(exists("/tmp/test-rename/file2.txt"), "new name exists");
    CHECK(read_verify("/tmp/test-rename/file2.txt", "hello"), "content preserved");

    /* Test 2: rename across subdirectories */
    mkdir_p("/tmp/test-rename/subA/dir1");
    mkdir_p("/tmp/test-rename/subA/dir2");
    CHECK(write_file("/tmp/test-rename/subA/dir1/src.txt", "cross-dir") == 0, "write cross-dir src");
    CHECK(rename("/tmp/test-rename/subA/dir1/src.txt",
                 "/tmp/test-rename/subA/dir2/dst.txt") == 0, "rename across subdirs");
    CHECK(!exists("/tmp/test-rename/subA/dir1/src.txt"), "cross-dir old gone");
    CHECK(exists("/tmp/test-rename/subA/dir2/dst.txt"), "cross-dir new exists");
    CHECK(read_verify("/tmp/test-rename/subA/dir2/dst.txt", "cross-dir"), "cross-dir content");

    /* Test 3: rename overwrites existing target */
    CHECK(write_file("/tmp/test-rename/subA/dir1/a.txt", "original") == 0, "write a");
    CHECK(write_file("/tmp/test-rename/subA/dir1/b.txt", "target") == 0, "write b");
    CHECK(rename("/tmp/test-rename/subA/dir1/a.txt",
                 "/tmp/test-rename/subA/dir1/b.txt") == 0, "rename overwrite");
    CHECK(!exists("/tmp/test-rename/subA/dir1/a.txt"), "overwrite old gone");

    /* Test 4: rename non-existent source returns ENOENT */
    if (rename("/tmp/test-rename/noexist", "/tmp/test-rename/xxx") == 0) {
        printf("FAIL: rename non-existent should fail\n"); return 1;
    }
    CHECK(errno == ENOENT, "rename non-existent -> ENOENT");

    /* Test 5: rename to non-existent directory returns ENOENT */
    CHECK(write_file("/tmp/test-rename/subA/dir1/x.txt", "x") == 0, "write x");
    if (rename("/tmp/test-rename/subA/dir1/x.txt",
               "/tmp/test-rename/no-dir/x.txt") == 0) {
        printf("FAIL: rename to bad dir should fail\n"); return 1;
    }
    CHECK(errno == ENOENT, "rename to bad dir -> ENOENT");

    /* Test 6: rename file from parent directory into child subdirectory
       (the actual bug: VFS ancestor check falsely rejected this) */
    mkdir_p("/tmp/test-rename/parent-dir/child");
    CHECK(write_file("/tmp/test-rename/parent-dir/parent-file.txt", "parent-child") == 0, "write parent-file");
    CHECK(rename("/tmp/test-rename/parent-dir/parent-file.txt",
                 "/tmp/test-rename/parent-dir/child/parent-file.txt") == 0, "rename file parent -> child");
    CHECK(!exists("/tmp/test-rename/parent-dir/parent-file.txt"), "parent-child old gone");
    CHECK(exists("/tmp/test-rename/parent-dir/child/parent-file.txt"), "parent-child new exists");
    CHECK(read_verify("/tmp/test-rename/parent-dir/child/parent-file.txt", "parent-child"), "parent-child content");

    cleanup();
    printf("\nALL RENAME TESTS PASSED\n");
    return 0;
}
