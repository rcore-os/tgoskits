/*！
 * bug-ext4-dir-ops.c
 *
 * Verifies POSIX rmdir() and rename() semantics on ext4 (rsext4):
 *
 *   rmdir:
 *     1. rmdir(non_empty_dir) == ENOTEMPTY  -- must not recursively delete
 *     2. rmdir(empty_dir)     == 0          -- must succeed
 *
 *   rename (target already exists):
 *     3. rename(dir, non_empty_dir) == ENOTEMPTY  -- no recursive delete
 *     4. rename(dir, empty_dir)     == 0          -- replace empty dir
 *     5. rename(file, dir)          == ENOTDIR    -- no cross-type overwrite
 *     6. rename(dir, file)          == EISDIR     -- no cross-type overwrite
 *     7. rename(file, file)         == 0          -- overwrite file
 *
 * Previous rmdir bug: DirNodeOps::unlink called rsext4::delete_dir()
 * (recursive rm -rf semantics) instead of checking emptiness first.
 *
 * Previous rename bug: rename() called delete_dir() on non-empty directory
 * targets (same recursive-delete issue), and lacked type cross-checks so
 * file-over-dir and dir-over-file overwrites silently destroyed data.
 *
 * Test path must be on ext4 root filesystem (/root), not /tmp (tmpfs),
 * because tmpfs correctly returns ENOTEMPTY and would mask the rsext4 bug.
 */

#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int passed;
static int failed;
static int bug_count;

#define CHECK(cond, msg)                                                \
    do {                                                                \
        if (cond) {                                                     \
            printf("  [OK]   %s\n", (msg));                            \
            passed++;                                                   \
        } else {                                                        \
            printf("  [FAIL] %s (errno=%d %s)\n",                      \
                   (msg), errno, strerror(errno));                      \
            failed++;                                                   \
        }                                                               \
    } while (0)

/*
 * Manual recursive removal — returns 0 on success, -1 on any failure.
 * Unlike system("rm -rf"), this propagates errors so we can detect
 * if the bug corrupts the filesystem during cleanup.
 */
static int manual_rmdir_recursive(const char *path)
{
    DIR *d = opendir(path);
    if (!d)
        return -1;

    struct dirent *ent;
    char child[512];
    int ret = 0;

    while ((ent = readdir(d)) != NULL) {
        if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0)
            continue;
        snprintf(child, sizeof(child), "%s/%s", path, ent->d_name);
        if (ent->d_type == DT_DIR) {
            if (manual_rmdir_recursive(child) != 0)
                ret = -1;
        } else {
            if (unlink(child) != 0)
                ret = -1;
        }
    }
    closedir(d);

    if (rmdir(path) != 0)
        ret = -1;
    return ret;
}

/*
 * Remove a file or directory tree at path, ignoring errors.
 * Used for cleanup between test scenarios where the filesystem
 * state may be corrupted by a bug.
 */
static void force_remove(const char *path)
{
    struct stat st;
    if (stat(path, &st) != 0)
        return;
    if (S_ISDIR(st.st_mode))
        manual_rmdir_recursive(path);
    else
        unlink(path);
}

/*
 * Try rmdir on a non-empty directory and verify ENOTEMPTY + no cascade.
 * Returns 1 if bug detected, 0 if correct behavior.
 */
static int test_rmdir_nonempty(const char *label, const char *dir_path,
                               const char *parent_dir, const char *sibling_path)
{
    printf("\n--- rmdir(non-empty): %s ---\n", label);
    printf("  target:   %s\n", dir_path);
    printf("  parent:   %s\n", parent_dir);
    printf("  sibling:  %s\n", sibling_path);

    /* Verify pre-conditions */
    CHECK(access(dir_path, F_OK) == 0, "target exists before rmdir");
    CHECK(access(sibling_path, F_OK) == 0, "sibling exists before rmdir");

    errno = 0;
    int rc = rmdir(dir_path);

    if (rc == 0) {
        printf("  [BUG]   rmdir succeeded on non-empty dir — recursive delete!\n");
        bug_count++;
        failed++;

        /* Check cascade damage */
        if (access(dir_path, F_OK) != 0)
            printf("  [BUG]   target/ was deleted\n");
        if (access(sibling_path, F_OK) != 0) {
            printf("  [BUG]   sibling was destroyed (cascading)\n");
            bug_count++;
        }
        if (access(parent_dir, F_OK) != 0) {
            printf("  [BUG]   parent/ was destroyed (cascading)\n");
            bug_count++;
        }
        return 1; /* bug detected */
    }

    if (errno == ENOTEMPTY || errno == EEXIST) {
        printf("  [OK]   returned ENOTEMPTY (rc=%d, errno=%d)\n", rc, errno);
        passed++;
    } else {
        printf("  [FAIL] unexpected errno=%d (%s)\n", errno, strerror(errno));
        failed++;
    }

    /* Verify no cascade damage */
    CHECK(access(dir_path, F_OK) == 0, "target still exists");
    CHECK(access(sibling_path, F_OK) == 0, "sibling survived");
    CHECK(access(parent_dir, F_OK) == 0, "parent survived");
    return 0;
}

/* ================================================================
 * RENAME TESTS
 *
 * POSIX rename() semantics when destination already exists:
 *   - rename(dir,  non_empty_dir) → ENOTEMPTY  (no recursive delete)
 *   - rename(dir,  empty_dir)     → 0          (replace empty dir)
 *   - rename(file, dir)           → ENOTDIR    (type mismatch)
 *   - rename(dir,  file)          → EISDIR     (type mismatch)
 *   - rename(file, file)          → 0          (overwrite)
 * ================================================================ */

/*
 * Test: rename dir → non-empty dir.
 * Expect: ENOTEMPTY, both directory trees intact.
 * Bug: rsext4 delete_dir() recursively destroys the target tree.
 */
static void test_rename_dir_to_nonempty_dir(const char *base)
{
    printf("\n--- rename(dir → non-empty dir) ---\n");

    char src_dir[256], src_file[256];
    char dst_dir[256], dst_file[256];

    snprintf(src_dir, sizeof(src_dir), "%s/rn_src_dir", base);
    snprintf(src_file, sizeof(src_file), "%s/rn_src_dir/data.txt", base);
    snprintf(dst_dir, sizeof(dst_dir), "%s/rn_dst_dir", base);
    snprintf(dst_file, sizeof(dst_file), "%s/rn_dst_dir/keep.txt", base);

    CHECK(mkdir(src_dir, 0755) == 0, "mkdir src_dir");
    CHECK(mkdir(dst_dir, 0755) == 0, "mkdir dst_dir");

    FILE *f = fopen(src_file, "w");
    CHECK(f != NULL, "create src_dir/data.txt");
    if (f) { fprintf(f, "source\n"); fclose(f); }

    f = fopen(dst_file, "w");
    CHECK(f != NULL, "create dst_dir/keep.txt");
    if (f) { fprintf(f, "target\n"); fclose(f); }

    errno = 0;
    int rc = rename(src_dir, dst_dir);

    if (rc == 0) {
        printf("  [BUG]   rename succeeded — recursive delete of non-empty dir!\n");
        bug_count++;
        failed++;
        if (access(dst_file, F_OK) != 0)
            printf("  [BUG]   dst_dir/keep.txt was destroyed\n");
        if (access(src_dir, F_OK) != 0)
            printf("  [BUG]   src_dir was removed\n");
    } else if (errno == ENOTEMPTY || errno == EEXIST) {
        printf("  [OK]   returned ENOTEMPTY (rc=%d, errno=%d)\n", rc, errno);
        passed++;
        CHECK(access(src_dir, F_OK) == 0, "src_dir still exists");
        CHECK(access(src_file, F_OK) == 0, "src_dir/data.txt intact");
        CHECK(access(dst_dir, F_OK) == 0, "dst_dir still exists");
        CHECK(access(dst_file, F_OK) == 0, "dst_dir/keep.txt intact");
    } else {
        printf("  [FAIL] unexpected errno=%d (%s)\n", errno, strerror(errno));
        failed++;
    }

    force_remove(src_dir);
    force_remove(dst_dir);
}

/*
 * Test: rename dir → empty dir.
 * Expect: success, old empty dir replaced by source dir.
 */
static void test_rename_dir_to_empty_dir(const char *base)
{
    printf("\n--- rename(dir → empty dir) ---\n");

    char src_dir[256], src_file[256];
    char dst_dir[256];
    char moved_file[256];

    snprintf(src_dir, sizeof(src_dir), "%s/rn_src2", base);
    snprintf(src_file, sizeof(src_file), "%s/rn_src2/moved.txt", base);
    snprintf(dst_dir, sizeof(dst_dir), "%s/rn_dst2", base);
    snprintf(moved_file, sizeof(moved_file), "%s/rn_dst2/moved.txt", base);

    CHECK(mkdir(src_dir, 0755) == 0, "mkdir src_dir");
    CHECK(mkdir(dst_dir, 0755) == 0, "mkdir dst_dir (empty)");

    FILE *f = fopen(src_file, "w");
    CHECK(f != NULL, "create src_dir/moved.txt");
    if (f) { fprintf(f, "payload\n"); fclose(f); }

    errno = 0;
    int rc = rename(src_dir, dst_dir);

    CHECK(rc == 0, "rename(dir, empty_dir) succeeded");
    if (rc == 0) {
        CHECK(access(src_dir, F_OK) != 0, "src_dir removed");
        CHECK(access(moved_file, F_OK) == 0, "moved.txt accessible at new path");
    }

    force_remove(dst_dir);
    force_remove(src_dir);
}

/*
 * Test: rename file → directory.
 * Expect: ENOTDIR, both source file and target dir intact.
 * Bug: rsext4 rename() deletes the target dir and moves file into its place.
 */
static void test_rename_file_to_dir(const char *base)
{
    printf("\n--- rename(file → dir) ---\n");

    char src_file[256], dst_dir[256], dst_child[256];

    snprintf(src_file, sizeof(src_file), "%s/rn_file.txt", base);
    snprintf(dst_dir, sizeof(dst_dir), "%s/rn_dir_target", base);
    snprintf(dst_child, sizeof(dst_child), "%s/rn_dir_target/child.txt", base);

    FILE *f = fopen(src_file, "w");
    CHECK(f != NULL, "create source file");
    if (f) { fprintf(f, "file\n"); fclose(f); }

    CHECK(mkdir(dst_dir, 0755) == 0, "mkdir target dir");
    f = fopen(dst_child, "w");
    CHECK(f != NULL, "create target dir/child.txt");
    if (f) { fprintf(f, "child\n"); fclose(f); }

    errno = 0;
    int rc = rename(src_file, dst_dir);

    if (rc == 0) {
        printf("  [BUG]   rename(file, dir) succeeded — dir destroyed!\n");
        bug_count++;
        failed++;
        if (access(dst_child, F_OK) != 0)
            printf("  [BUG]   target dir contents destroyed\n");
    } else if (errno == ENOTDIR || errno == EISDIR) {
        printf("  [OK]   returned ENOTDIR/EISDIR (rc=%d, errno=%d)\n", rc, errno);
        passed++;
        CHECK(access(src_file, F_OK) == 0, "source file still exists");
        CHECK(access(dst_dir, F_OK) == 0, "target dir still exists");
        CHECK(access(dst_child, F_OK) == 0, "target dir contents intact");
    } else {
        printf("  [FAIL] unexpected errno=%d (%s)\n", errno, strerror(errno));
        failed++;
    }

    force_remove(dst_dir);
    force_remove(src_file);
}

/*
 * Test: rename directory → file.
 * Expect: EISDIR, both source dir and target file intact.
 * Bug: rsext4 rename() deletes the target file and moves dir into its place.
 */
static void test_rename_dir_to_file(const char *base)
{
    printf("\n--- rename(dir → file) ---\n");

    char src_dir[256], src_child[256], dst_file[256];

    snprintf(src_dir, sizeof(src_dir), "%s/rn_dir_src", base);
    snprintf(src_child, sizeof(src_child), "%s/rn_dir_src/inside.txt", base);
    snprintf(dst_file, sizeof(dst_file), "%s/rn_file_target.txt", base);

    CHECK(mkdir(src_dir, 0755) == 0, "mkdir source dir");

    FILE *f = fopen(src_child, "w");
    CHECK(f != NULL, "create source dir/inside.txt");
    if (f) { fprintf(f, "inside\n"); fclose(f); }

    f = fopen(dst_file, "w");
    CHECK(f != NULL, "create target file");
    if (f) { fprintf(f, "target\n"); fclose(f); }

    errno = 0;
    int rc = rename(src_dir, dst_file);

    if (rc == 0) {
        printf("  [BUG]   rename(dir, file) succeeded — dir overwrote file!\n");
        bug_count++;
        failed++;
    } else if (errno == EISDIR || errno == ENOTDIR) {
        printf("  [OK]   returned EISDIR (rc=%d, errno=%d)\n", rc, errno);
        passed++;
        CHECK(access(src_dir, F_OK) == 0, "source dir still exists");
        CHECK(access(src_child, F_OK) == 0, "source dir contents intact");
        CHECK(access(dst_file, F_OK) == 0, "target file still exists");
    } else {
        printf("  [FAIL] unexpected errno=%d (%s)\n", errno, strerror(errno));
        failed++;
    }

    force_remove(src_dir);
    force_remove(dst_file);
}

/*
 * Test: rename file → file (overwrite).
 * Expect: success, old file replaced by source file.
 */
static void test_rename_file_to_file(const char *base)
{
    printf("\n--- rename(file → file) ---\n");

    char src[256], dst[256];

    snprintf(src, sizeof(src), "%s/rn_f2f_src.txt", base);
    snprintf(dst, sizeof(dst), "%s/rn_f2f_dst.txt", base);

    FILE *f = fopen(src, "w");
    CHECK(f != NULL, "create source file");
    if (f) { fprintf(f, "new-content\n"); fclose(f); }

    f = fopen(dst, "w");
    CHECK(f != NULL, "create destination file");
    if (f) { fprintf(f, "old-content\n"); fclose(f); }

    errno = 0;
    int rc = rename(src, dst);

    CHECK(rc == 0, "rename(file, file) succeeded");
    if (rc == 0) {
        CHECK(access(src, F_OK) != 0, "source removed");
        CHECK(access(dst, F_OK) == 0, "destination exists");

        /* Verify content was overwritten */
        f = fopen(dst, "r");
        if (f) {
            char buf[64] = {0};
            fread(buf, 1, sizeof(buf) - 1, f);
            fclose(f);
            CHECK(strstr(buf, "new-content") != NULL, "content is from source file");
        }
    }

    force_remove(src);
    force_remove(dst);
}

/* ================================================================
 * MAIN
 * ================================================================ */

int main(void)
{
    const char *base = "/root/bug-ext4-dir-ops-test";
    char parent[256], sibling[256], empty_dir[256];

    printf("=== bug-ext4-dir-ops ===\n");

    /* Cleanup from previous runs */
    manual_rmdir_recursive(base);

    /* Create base structure */
    snprintf(parent, sizeof(parent), "%s/parent", base);
    snprintf(sibling, sizeof(sibling), "%s/parent/sibling.txt", base);
    snprintf(empty_dir, sizeof(empty_dir), "%s/parent/empty", base);

    CHECK(mkdir(base, 0755) == 0, "mkdir base");
    CHECK(mkdir(parent, 0755) == 0, "mkdir parent");

    FILE *f = fopen(sibling, "w");
    CHECK(f != NULL, "create sibling.txt");
    if (f) { fprintf(f, "sibling\n"); fclose(f); }

    CHECK(mkdir(empty_dir, 0755) == 0, "mkdir empty (control group)");

    /* ==============================================================
     * RMDR TESTS
     * ============================================================== */

    /*
     * === Test 1: dir with files + subdirectory (mixed) ===
     *
     *   parent/
     *     target_mixed/
     *       file1.txt
     *       file2.txt
     *       sub/
     *         deep.txt
     *     sibling.txt
     */
    {
        char tdir[256], sub[256], f1[256], f2[256], deep[256];
        snprintf(tdir, sizeof(tdir), "%s/parent/target_mixed", base);
        snprintf(sub, sizeof(sub), "%s/parent/target_mixed/sub", base);
        snprintf(f1, sizeof(f1), "%s/parent/target_mixed/file1.txt", base);
        snprintf(f2, sizeof(f2), "%s/parent/target_mixed/file2.txt", base);
        snprintf(deep, sizeof(deep), "%s/parent/target_mixed/sub/deep.txt", base);

        CHECK(mkdir(tdir, 0755) == 0, "mkdir target_mixed");
        CHECK(mkdir(sub, 0755) == 0, "mkdir target_mixed/sub");

        f = fopen(f1, "w");
        CHECK(f != NULL, "create file1.txt");
        if (f) { fprintf(f, "hello\n"); fclose(f); }
        f = fopen(f2, "w");
        CHECK(f != NULL, "create file2.txt");
        if (f) { fprintf(f, "world\n"); fclose(f); }
        f = fopen(deep, "w");
        CHECK(f != NULL, "create sub/deep.txt");
        if (f) { fprintf(f, "deep\n"); fclose(f); }

        test_rmdir_nonempty("mixed (files+subdir)", tdir, parent, sibling);

        /* Verify deep content survived */
        CHECK(access(deep, F_OK) == 0, "sub/deep.txt intact");

        /* Cleanup for next test */
        manual_rmdir_recursive(tdir);
    }

    /*
     * === Test 2: dir with files only (no subdirectories) ===
     *
     *   parent/
     *     target_files/
     *       a.txt
     *       b.txt
     *       c.txt
     *     sibling.txt
     */
    {
        char tdir[256], fa[256], fb[256], fc[256];
        snprintf(tdir, sizeof(tdir), "%s/parent/target_files", base);
        snprintf(fa, sizeof(fa), "%s/parent/target_files/a.txt", base);
        snprintf(fb, sizeof(fb), "%s/parent/target_files/b.txt", base);
        snprintf(fc, sizeof(fc), "%s/parent/target_files/c.txt", base);

        CHECK(mkdir(tdir, 0755) == 0, "mkdir target_files");

        f = fopen(fa, "w");
        CHECK(f != NULL, "create a.txt");
        if (f) { fprintf(f, "a\n"); fclose(f); }
        f = fopen(fb, "w");
        CHECK(f != NULL, "create b.txt");
        if (f) { fprintf(f, "b\n"); fclose(f); }
        f = fopen(fc, "w");
        CHECK(f != NULL, "create c.txt");
        if (f) { fprintf(f, "c\n"); fclose(f); }

        test_rmdir_nonempty("files only", tdir, parent, sibling);
        CHECK(access(fa, F_OK) == 0, "a.txt intact");
        CHECK(access(fb, F_OK) == 0, "b.txt intact");
        CHECK(access(fc, F_OK) == 0, "c.txt intact");

        manual_rmdir_recursive(tdir);
    }

    /*
     * === Test 3: dir with subdirectories only (no files) ===
     *
     *   parent/
     *     target_subdirs/
     *       child1/
     *         dummy.txt
     *       child2/
     *         dummy.txt
     *     sibling.txt
     */
    {
        char tdir[256], c1[256], c2[256], d1[256], d2[256];
        snprintf(tdir, sizeof(tdir), "%s/parent/target_subdirs", base);
        snprintf(c1, sizeof(c1), "%s/parent/target_subdirs/child1", base);
        snprintf(c2, sizeof(c2), "%s/parent/target_subdirs/child2", base);
        snprintf(d1, sizeof(d1), "%s/parent/target_subdirs/child1/dummy.txt", base);
        snprintf(d2, sizeof(d2), "%s/parent/target_subdirs/child2/dummy.txt", base);

        CHECK(mkdir(tdir, 0755) == 0, "mkdir target_subdirs");
        CHECK(mkdir(c1, 0755) == 0, "mkdir child1");
        CHECK(mkdir(c2, 0755) == 0, "mkdir child2");

        f = fopen(d1, "w");
        CHECK(f != NULL, "create child1/dummy.txt");
        if (f) { fprintf(f, "d1\n"); fclose(f); }
        f = fopen(d2, "w");
        CHECK(f != NULL, "create child2/dummy.txt");
        if (f) { fprintf(f, "d2\n"); fclose(f); }

        test_rmdir_nonempty("subdirs only", tdir, parent, sibling);
        CHECK(access(d1, F_OK) == 0, "child1/dummy.txt intact");
        CHECK(access(d2, F_OK) == 0, "child2/dummy.txt intact");

        manual_rmdir_recursive(tdir);
    }

    /*
     * === Test 4: rmdir on parent (which contains sibling.txt) ===
     *
     * This tests cascade upward: if delete_dir is truly recursive,
     * attempting to rmdir parent/ should destroy base/ as well.
     */
    {
        printf("\n--- rmdir(non-empty): parent dir ---\n");
        CHECK(access(parent, F_OK) == 0, "parent exists");
        CHECK(access(sibling, F_OK) == 0, "sibling exists");

        errno = 0;
        int rc = rmdir(parent);

        if (rc == 0) {
            printf("  [BUG]   rmdir(parent) succeeded — cascade to parent!\n");
            bug_count++;
            failed++;
            if (access(base, F_OK) != 0) {
                printf("  [BUG]   base/ was also destroyed (cascade upward)\n");
                bug_count++;
            }
        } else if (errno == ENOTEMPTY || errno == EEXIST) {
            printf("  [OK]   rmdir(parent) correctly returned ENOTEMPTY\n");
            passed++;
        } else {
            printf("  [FAIL] unexpected errno=%d (%s)\n", errno, strerror(errno));
            failed++;
        }

        CHECK(access(parent, F_OK) == 0, "parent still exists");
        CHECK(access(sibling, F_OK) == 0, "sibling still exists");
        CHECK(access(base, F_OK) == 0, "base still exists");
    }

    /*
     * === Test 5: rmdir on empty directory (control group) ===
     */
    printf("\n--- rmdir(empty dir) ---\n");
    {
        int rc = rmdir(empty_dir);
        CHECK(rc == 0, "rmdir(empty) succeeded");
        CHECK(access(empty_dir, F_OK) != 0, "empty dir removed");
    }

    /*
     * === Test 6: repeated rmdir on same path ===
     *
     * After successful rmdir, calling again should return ENOENT.
     * If the bug corrupts internal state, this might crash or return
     * unexpected errors.
     */
    printf("\n--- repeated rmdir on removed dir ---\n");
    {
        errno = 0;
        int rc = rmdir(empty_dir);
        CHECK(rc != 0, "second rmdir(empty) fails");
        CHECK(errno == ENOENT, "second rmdir returns ENOENT");
    }

    /* ==============================================================
     * RENAME TESTS
     * ============================================================== */

    test_rename_dir_to_nonempty_dir(base);
    test_rename_dir_to_empty_dir(base);
    test_rename_file_to_dir(base);
    test_rename_dir_to_file(base);
    test_rename_file_to_file(base);

    /* Final cleanup */
    manual_rmdir_recursive(base);

    /* === Summary === */
    printf("\n=== result: %d passed, %d failed, %d bugs ===\n",
           passed, failed, bug_count);
    if (failed == 0)
        printf("TEST PASSED\n");
    else
        printf("TEST FAILED\n");

    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
