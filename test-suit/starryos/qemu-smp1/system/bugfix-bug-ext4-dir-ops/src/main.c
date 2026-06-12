/*!
 * bug-ext4-dir-ops.c
 *！
 * Verifies POSIX rmdir() and rename() semantics on ext4 (rsext4),
 * plus VFS dentry cache correctness after rename (stale parent bug).
 *
 * Test path must be on ext4 root filesystem (/root), not /tmp (tmpfs),
 * because tmpfs correctly returns ENOTEMPTY and would mask the rsext4 bug.
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <dirent.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

/* linux_dirent64 for getdents64 syscall */
struct linux_dirent64 {
    unsigned long long d_ino;
    long long          d_off;
    unsigned short     d_reclen;
    unsigned char      d_type;
    char               d_name[];
};

#define BASE "/root/bug-ext4-dir-ops-test"
#define RENAME_MANY_FILE_COUNT 8
#define READDIR_DELETE_FILE_COUNT 8
#define RM_RF_FILE_COUNT 8

/* ========== 辅助函数 ========== */

static int write_file(const char *path, const char *data)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0)
        return -1;
    size_t len = strlen(data);
    ssize_t w = write(fd, data, len);
    close(fd);
    return (w == (ssize_t)len) ? 0 : -1;
}

static int read_file(const char *path, char *buf, int bufsz)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        return -1;
    int n = (int)read(fd, buf, bufsz - 1);
    close(fd);
    if (n < 0)
        return -1;
    buf[n] = '\0';
    return n;
}

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

static int count_dir_entries(const char *path)
{
    DIR *d = opendir(path);
    if (!d)
        return -1;
    int count = 0;
    struct dirent *ent;
    while ((ent = readdir(d)) != NULL) {
        if (strcmp(ent->d_name, ".") && strcmp(ent->d_name, ".."))
            count++;
    }
    closedir(d);
    return count;
}

static int dir_has_entry(const char *dir_path, const char *name)
{
    DIR *d = opendir(dir_path);
    if (!d)
        return 0;
    struct dirent *ent;
    int found = 0;
    while ((ent = readdir(d)) != NULL) {
        if (strcmp(ent->d_name, name) == 0) {
            found = 1;
            break;
        }
    }
    closedir(d);
    return found;
}

/* ========== rmdir 测试 ========== */

static void test_rmdir_nonempty_mixed(void)
{
    char tdir[256], sub[256], f1[256], f2[256], deep[256];
    char parent[256], sibling[256];

    snprintf(parent, sizeof(parent), "%s/parent", BASE);
    snprintf(sibling, sizeof(sibling), "%s/parent/sibling.txt", BASE);
    snprintf(tdir, sizeof(tdir), "%s/parent/target_mixed", BASE);
    snprintf(sub, sizeof(sub), "%s/parent/target_mixed/sub", BASE);
    snprintf(f1, sizeof(f1), "%s/parent/target_mixed/file1.txt", BASE);
    snprintf(f2, sizeof(f2), "%s/parent/target_mixed/file2.txt", BASE);
    snprintf(deep, sizeof(deep), "%s/parent/target_mixed/sub/deep.txt", BASE);

    CHECK(mkdir(tdir, 0755) == 0, "mkdir target_mixed");
    CHECK(mkdir(sub, 0755) == 0, "mkdir target_mixed/sub");
    CHECK(write_file(f1, "hello\n") == 0, "create file1.txt");
    CHECK(write_file(f2, "world\n") == 0, "create file2.txt");
    CHECK(write_file(deep, "deep\n") == 0, "create sub/deep.txt");

    CHECK_ERR(rmdir(tdir), ENOTEMPTY, "rmdir(non-empty mixed) -> ENOTEMPTY");
    CHECK(access(tdir, F_OK) == 0, "target still exists after rmdir");
    CHECK(access(sibling, F_OK) == 0, "sibling survived");
    CHECK(access(deep, F_OK) == 0, "sub/deep.txt intact");

    manual_rmdir_recursive(tdir);
}

static void test_rmdir_nonempty_files(void)
{
    char tdir[256], fa[256], fb[256], fc[256];

    snprintf(tdir, sizeof(tdir), "%s/parent/target_files", BASE);
    snprintf(fa, sizeof(fa), "%s/parent/target_files/a.txt", BASE);
    snprintf(fb, sizeof(fb), "%s/parent/target_files/b.txt", BASE);
    snprintf(fc, sizeof(fc), "%s/parent/target_files/c.txt", BASE);

    CHECK(mkdir(tdir, 0755) == 0, "mkdir target_files");
    CHECK(write_file(fa, "a\n") == 0, "create a.txt");
    CHECK(write_file(fb, "b\n") == 0, "create b.txt");
    CHECK(write_file(fc, "c\n") == 0, "create c.txt");

    CHECK_ERR(rmdir(tdir), ENOTEMPTY, "rmdir(non-empty files) -> ENOTEMPTY");
    CHECK(access(fa, F_OK) == 0, "a.txt intact");
    CHECK(access(fb, F_OK) == 0, "b.txt intact");
    CHECK(access(fc, F_OK) == 0, "c.txt intact");

    manual_rmdir_recursive(tdir);
}

static void test_rmdir_nonempty_subdirs(void)
{
    char tdir[256], c1[256], c2[256], d1[256], d2[256];

    snprintf(tdir, sizeof(tdir), "%s/parent/target_subdirs", BASE);
    snprintf(c1, sizeof(c1), "%s/parent/target_subdirs/child1", BASE);
    snprintf(c2, sizeof(c2), "%s/parent/target_subdirs/child2", BASE);
    snprintf(d1, sizeof(d1), "%s/parent/target_subdirs/child1/dummy.txt", BASE);
    snprintf(d2, sizeof(d2), "%s/parent/target_subdirs/child2/dummy.txt", BASE);

    CHECK(mkdir(tdir, 0755) == 0, "mkdir target_subdirs");
    CHECK(mkdir(c1, 0755) == 0, "mkdir child1");
    CHECK(mkdir(c2, 0755) == 0, "mkdir child2");
    CHECK(write_file(d1, "d1\n") == 0, "create child1/dummy.txt");
    CHECK(write_file(d2, "d2\n") == 0, "create child2/dummy.txt");

    CHECK_ERR(rmdir(tdir), ENOTEMPTY, "rmdir(non-empty subdirs) -> ENOTEMPTY");
    CHECK(access(d1, F_OK) == 0, "child1/dummy.txt intact");
    CHECK(access(d2, F_OK) == 0, "child2/dummy.txt intact");

    manual_rmdir_recursive(tdir);
}

static void test_rmdir_nonempty_parent(void)
{
    char parent[256], sibling[256];

    snprintf(parent, sizeof(parent), "%s/parent", BASE);
    snprintf(sibling, sizeof(sibling), "%s/parent/sibling.txt", BASE);

    CHECK(access(parent, F_OK) == 0, "parent exists");
    CHECK(access(sibling, F_OK) == 0, "sibling exists");
    CHECK_ERR(rmdir(parent), ENOTEMPTY, "rmdir(parent) -> ENOTEMPTY");
    CHECK(access(parent, F_OK) == 0, "parent still exists");
    CHECK(access(sibling, F_OK) == 0, "sibling still exists");
}

static void test_rmdir_empty(void)
{
    char empty_dir[256];
    snprintf(empty_dir, sizeof(empty_dir), "%s/parent/empty", BASE);

    CHECK_RET(rmdir(empty_dir), 0, "rmdir(empty) succeeds");
    CHECK(access(empty_dir, F_OK) != 0, "empty dir removed");

    /* repeated rmdir should return ENOENT */
    CHECK_ERR(rmdir(empty_dir), ENOENT, "second rmdir(empty) -> ENOENT");
}

/* ========== rename 基本语义测试 ========== */

static void test_rename_dir_to_nonempty_dir(void)
{
    char src_dir[256], src_file[256], dst_dir[256], dst_file[256];

    snprintf(src_dir, sizeof(src_dir), "%s/rn_src_dir", BASE);
    snprintf(src_file, sizeof(src_file), "%s/rn_src_dir/data.txt", BASE);
    snprintf(dst_dir, sizeof(dst_dir), "%s/rn_dst_dir", BASE);
    snprintf(dst_file, sizeof(dst_file), "%s/rn_dst_dir/keep.txt", BASE);

    CHECK(mkdir(src_dir, 0755) == 0, "mkdir src_dir");
    CHECK(mkdir(dst_dir, 0755) == 0, "mkdir dst_dir");
    CHECK(write_file(src_file, "source\n") == 0, "create src_dir/data.txt");
    CHECK(write_file(dst_file, "target\n") == 0, "create dst_dir/keep.txt");

    CHECK_ERR(rename(src_dir, dst_dir), ENOTEMPTY,
              "rename(dir, non-empty dir) -> ENOTEMPTY");
    CHECK(access(src_dir, F_OK) == 0, "src_dir still exists");
    CHECK(access(src_file, F_OK) == 0, "src_dir/data.txt intact");
    CHECK(access(dst_dir, F_OK) == 0, "dst_dir still exists");
    CHECK(access(dst_file, F_OK) == 0, "dst_dir/keep.txt intact");

    force_remove(src_dir);
    force_remove(dst_dir);
}

static void test_rename_dir_to_empty_dir(void)
{
    char src_dir[256], src_file[256], dst_dir[256], moved_file[256];

    snprintf(src_dir, sizeof(src_dir), "%s/rn_src2", BASE);
    snprintf(src_file, sizeof(src_file), "%s/rn_src2/moved.txt", BASE);
    snprintf(dst_dir, sizeof(dst_dir), "%s/rn_dst2", BASE);
    snprintf(moved_file, sizeof(moved_file), "%s/rn_dst2/moved.txt", BASE);

    CHECK(mkdir(src_dir, 0755) == 0, "mkdir src_dir");
    CHECK(mkdir(dst_dir, 0755) == 0, "mkdir dst_dir (empty)");
    CHECK(write_file(src_file, "payload\n") == 0, "create src_dir/moved.txt");

    CHECK_RET(rename(src_dir, dst_dir), 0, "rename(dir, empty_dir) succeeds");
    CHECK(access(src_dir, F_OK) != 0, "src_dir removed");
    CHECK(access(moved_file, F_OK) == 0, "moved.txt accessible at new path");

    force_remove(dst_dir);
    force_remove(src_dir);
}

static void test_rename_file_to_dir(void)
{
    char src_file[256], dst_dir[256], dst_child[256];

    snprintf(src_file, sizeof(src_file), "%s/rn_file.txt", BASE);
    snprintf(dst_dir, sizeof(dst_dir), "%s/rn_dir_target", BASE);
    snprintf(dst_child, sizeof(dst_child), "%s/rn_dir_target/child.txt", BASE);

    CHECK(write_file(src_file, "file\n") == 0, "create source file");
    CHECK(mkdir(dst_dir, 0755) == 0, "mkdir target dir");
    CHECK(write_file(dst_child, "child\n") == 0, "create target dir/child.txt");

    CHECK(rename(src_file, dst_dir) != 0, "rename(file, dir) fails");
    CHECK(errno == ENOTDIR || errno == EISDIR,
          "errno is ENOTDIR or EISDIR");
    CHECK(access(src_file, F_OK) == 0, "source file still exists");
    CHECK(access(dst_dir, F_OK) == 0, "target dir still exists");
    CHECK(access(dst_child, F_OK) == 0, "target dir contents intact");

    force_remove(dst_dir);
    force_remove(src_file);
}

static void test_rename_dir_to_file(void)
{
    char src_dir[256], src_child[256], dst_file[256];

    snprintf(src_dir, sizeof(src_dir), "%s/rn_dir_src", BASE);
    snprintf(src_child, sizeof(src_child), "%s/rn_dir_src/inside.txt", BASE);
    snprintf(dst_file, sizeof(dst_file), "%s/rn_file_target.txt", BASE);

    CHECK(mkdir(src_dir, 0755) == 0, "mkdir source dir");
    CHECK(write_file(src_child, "inside\n") == 0, "create source dir/inside.txt");
    CHECK(write_file(dst_file, "target\n") == 0, "create target file");

    CHECK(rename(src_dir, dst_file) != 0, "rename(dir, file) fails");
    CHECK(errno == EISDIR || errno == ENOTDIR,
          "errno is EISDIR or ENOTDIR");
    CHECK(access(src_dir, F_OK) == 0, "source dir still exists");
    CHECK(access(src_child, F_OK) == 0, "source dir contents intact");
    CHECK(access(dst_file, F_OK) == 0, "target file still exists");

    force_remove(src_dir);
    force_remove(dst_file);
}

static void test_rename_file_to_file(void)
{
    char src[256], dst[256], buf[64];

    snprintf(src, sizeof(src), "%s/rn_f2f_src.txt", BASE);
    snprintf(dst, sizeof(dst), "%s/rn_f2f_dst.txt", BASE);

    CHECK(write_file(src, "new-content\n") == 0, "create source file");
    CHECK(write_file(dst, "old-content\n") == 0, "create destination file");

    CHECK_RET(rename(src, dst), 0, "rename(file, file) succeeds");
    CHECK(access(src, F_OK) != 0, "source removed");
    CHECK(access(dst, F_OK) == 0, "destination exists");

    memset(buf, 0, sizeof(buf));
    CHECK(read_file(dst, buf, sizeof(buf)) > 0, "read destination");
    CHECK(strstr(buf, "new-content") != NULL, "content is from source file");

    force_remove(src);
    force_remove(dst);
}

/* ========== rename + VFS dentry cache 测试 ========== */

static void test_rename_then_unlink(void)
{
    char dir[256], child[256], renamed[256], renamed_child[256];

    snprintf(dir, sizeof(dir), "%s/rnu_old", BASE);
    snprintf(child, sizeof(child), "%s/rnu_old/data.txt", BASE);
    snprintf(renamed, sizeof(renamed), "%s/rnu_new", BASE);
    snprintf(renamed_child, sizeof(renamed_child), "%s/rnu_new/data.txt", BASE);

    CHECK(mkdir(dir, 0755) == 0, "mkdir old dir");
    CHECK(write_file(child, "hello\n") == 0, "create old/data.txt");
    CHECK_RET(rename(dir, renamed), 0, "rename old -> ~new");

    /* stale parent bug: unlink via new path should succeed */
    CHECK_RET(unlink(renamed_child), 0, "unlink ~new/data.txt");
    CHECK_RET(rmdir(renamed), 0, "rmdir ~new after unlink");

    force_remove(renamed);
    force_remove(dir);
}

static void test_rename_file_then_delete_old_path(void)
{
    char dir[256], old_path[256], new_path[256];

    snprintf(dir, sizeof(dir), "%s/rnfd_dir", BASE);
    snprintf(old_path, sizeof(old_path), "%s/rnfd_dir/old_name.txt", BASE);
    snprintf(new_path, sizeof(new_path), "%s/rnfd_dir/new_name.txt", BASE);

    CHECK(mkdir(dir, 0755) == 0, "mkdir test dir");
    CHECK(write_file(old_path, "data\n") == 0, "create old_name.txt");
    CHECK_RET(rename(old_path, new_path), 0, "rename old_name -> new_name");

    /* old path must be gone */
    CHECK(access(old_path, F_OK) != 0, "old_name.txt no longer accessible");
    CHECK_ERR(unlink(old_path), ENOENT, "unlink old_name -> ENOENT");

    /* new path must work */
    CHECK_RET(unlink(new_path), 0, "unlink new_name.txt succeeds");
    CHECK_RET(rmdir(dir), 0, "rmdir empty dir");

    force_remove(dir);
}

static void test_rename_dir_then_delete_child_old_path(void)
{
    char parent[256], old_dir[256], old_child[256];
    char new_dir[256], new_child[256];

    snprintf(parent, sizeof(parent), "%s/rndp_parent", BASE);
    snprintf(old_dir, sizeof(old_dir), "%s/rndp_parent/old_pkg", BASE);
    snprintf(old_child, sizeof(old_child), "%s/rndp_parent/old_pkg/data.txt", BASE);
    snprintf(new_dir, sizeof(new_dir), "%s/rndp_parent/~new_pkg", BASE);
    snprintf(new_child, sizeof(new_child), "%s/rndp_parent/~new_pkg/data.txt", BASE);

    CHECK(mkdir(parent, 0755) == 0, "mkdir parent");
    CHECK(mkdir(old_dir, 0755) == 0, "mkdir old_pkg");
    CHECK(write_file(old_child, "content\n") == 0, "create old_pkg/data.txt");
    CHECK_RET(rename(old_dir, new_dir), 0, "rename old_pkg -> ~new_pkg");

    /* old path must be gone */
    CHECK(access(old_child, F_OK) != 0, "old_pkg/data.txt no longer accessible");
    CHECK_ERR(unlink(old_child), ENOENT, "unlink old_pkg/data.txt -> ENOENT");

    /* new path must work */
    CHECK_RET(unlink(new_child), 0, "unlink ~new_pkg/data.txt succeeds");
    CHECK_RET(rmdir(new_dir), 0, "rmdir ~new_pkg");
    CHECK_RET(rmdir(parent), 0, "rmdir parent");

    force_remove(parent);
}

static void test_rename_then_readdir(void)
{
    char dir[256], f1[256], f2[256], renamed[256], rpath[256], buf[64];

    snprintf(dir, sizeof(dir), "%s/rnr_old", BASE);
    snprintf(f1, sizeof(f1), "%s/rnr_old/a.txt", BASE);
    snprintf(f2, sizeof(f2), "%s/rnr_old/b.txt", BASE);
    snprintf(renamed, sizeof(renamed), "%s/rnr_new", BASE);

    CHECK(mkdir(dir, 0755) == 0, "mkdir old dir");
    CHECK(write_file(f1, "aaa\n") == 0, "create a.txt");
    CHECK(write_file(f2, "bbb\n") == 0, "create b.txt");
    CHECK_RET(rename(dir, renamed), 0, "rename old -> ~new");

    /* readdir must see both files */
    CHECK(dir_has_entry(renamed, "a.txt"), "readdir ~new sees a.txt");
    CHECK(dir_has_entry(renamed, "b.txt"), "readdir ~new sees b.txt");
    CHECK(count_dir_entries(renamed) == 2, "readdir ~new sees exactly 2 entries");

    /* read content through renamed path */
    snprintf(rpath, sizeof(rpath), "%s/a.txt", renamed);
    memset(buf, 0, sizeof(buf));
    CHECK(read_file(rpath, buf, sizeof(buf)) > 0, "read ~new/a.txt");
    CHECK(strcmp(buf, "aaa\n") == 0, "~new/a.txt content correct");

    force_remove(renamed);
    force_remove(dir);
}

static void test_rename_create_same_name(void)
{
    char old_dir[256], old_child[256];
    char renamed_dir[256], renamed_child[256];
    char new_dir[256], new_child[256], buf[64];

    snprintf(old_dir, sizeof(old_dir), "%s/rnc_pkg", BASE);
    snprintf(old_child, sizeof(old_child), "%s/rnc_pkg/init.py", BASE);
    snprintf(renamed_dir, sizeof(renamed_dir), "%s/rnc_~pkg", BASE);
    snprintf(renamed_child, sizeof(renamed_child), "%s/rnc_~pkg/init.py", BASE);
    snprintf(new_dir, sizeof(new_dir), "%s/rnc_pkg", BASE);
    snprintf(new_child, sizeof(new_child), "%s/rnc_pkg/main.py", BASE);

    CHECK(mkdir(old_dir, 0755) == 0, "mkdir old pkg");
    CHECK(write_file(old_child, "old\n") == 0, "create old/init.py");
    CHECK_RET(rename(old_dir, renamed_dir), 0, "rename pkg -> ~pkg");

    /* create new package with same name */
    CHECK(mkdir(new_dir, 0755) == 0, "mkdir new pkg (same name)");
    CHECK(write_file(new_child, "new\n") == 0, "create new/main.py");

    /* unlink inside renamed dir — stale parent bug target */
    CHECK_RET(unlink(renamed_child), 0, "unlink ~pkg/init.py");
    CHECK_RET(rmdir(renamed_dir), 0, "rmdir ~pkg");

    /* verify new package intact */
    memset(buf, 0, sizeof(buf));
    CHECK(read_file(new_child, buf, sizeof(buf)) > 0, "read new/main.py");
    CHECK(strcmp(buf, "new\n") == 0, "new/main.py intact after cleanup");
    CHECK(dir_has_entry(new_dir, "main.py"), "new pkg readdir sees main.py");

    force_remove(new_dir);
    force_remove(renamed_dir);
    force_remove(old_dir);
}

static void test_pip_upgrade_pattern(void)
{
    char pkg[256], init_f[256], main_f[256], sub[256], sub_f[256];
    char renamed[256], renamed_init[256], renamed_main[256], renamed_sub[256], renamed_sub_f[256];
    char new_pkg[256], new_init[256], new_main[256], new_cli[256], buf[64];

    snprintf(pkg, sizeof(pkg), "%s/pip", BASE);
    snprintf(init_f, sizeof(init_f), "%s/pip/__init__.py", BASE);
    snprintf(main_f, sizeof(main_f), "%s/pip/main.py", BASE);
    snprintf(sub, sizeof(sub), "%s/pip/cli", BASE);
    snprintf(sub_f, sizeof(sub_f), "%s/pip/cli/run.py", BASE);
    snprintf(renamed, sizeof(renamed), "%s/~ip", BASE);
    snprintf(renamed_init, sizeof(renamed_init), "%s/~ip/__init__.py", BASE);
    snprintf(renamed_main, sizeof(renamed_main), "%s/~ip/main.py", BASE);
    snprintf(renamed_sub, sizeof(renamed_sub), "%s/~ip/cli", BASE);
    snprintf(renamed_sub_f, sizeof(renamed_sub_f), "%s/~ip/cli/run.py", BASE);
    snprintf(new_pkg, sizeof(new_pkg), "%s/pip", BASE);
    snprintf(new_init, sizeof(new_init), "%s/pip/__init__.py", BASE);
    snprintf(new_main, sizeof(new_main), "%s/pip/main.py", BASE);
    snprintf(new_cli, sizeof(new_cli), "%s/pip/cli", BASE);

    /* old package */
    CHECK(mkdir(pkg, 0755) == 0, "mkdir pip");
    CHECK(write_file(init_f, "# old init\n") == 0, "create __init__.py");
    CHECK(write_file(main_f, "# old main\n") == 0, "create main.py");
    CHECK(mkdir(sub, 0755) == 0, "mkdir cli");
    CHECK(write_file(sub_f, "# old cli\n") == 0, "create cli/run.py");

    /* rename pip -> ~ip */
    CHECK_RET(rename(pkg, renamed), 0, "rename pip -> ~ip");

    /* install new version */
    CHECK(mkdir(new_pkg, 0755) == 0, "mkdir new pip");
    CHECK(write_file(new_init, "# new init\n") == 0, "create new __init__.py");
    CHECK(write_file(new_main, "# new main\n") == 0, "create new main.py");
    CHECK(mkdir(new_cli, 0755) == 0, "mkdir new cli");

    /* cleanup ~ip */
    CHECK_RET(unlink(renamed_init), 0, "unlink ~ip/__init__.py");
    CHECK_RET(unlink(renamed_main), 0, "unlink ~ip/main.py");
    CHECK_RET(unlink(renamed_sub_f), 0, "unlink ~ip/cli/run.py");
    CHECK_RET(rmdir(renamed_sub), 0, "rmdir ~ip/cli");
    CHECK_RET(rmdir(renamed), 0, "rmdir ~ip");

    /* verify new package */
    memset(buf, 0, sizeof(buf));
    CHECK(read_file(new_main, buf, sizeof(buf)) > 0, "read new pip/main.py");
    CHECK(strcmp(buf, "# new main\n") == 0, "new pip/main.py intact");
    memset(buf, 0, sizeof(buf));
    CHECK(read_file(new_init, buf, sizeof(buf)) > 0, "read new pip/__init__.py");
    CHECK(strcmp(buf, "# new init\n") == 0, "new pip/__init__.py intact");

    force_remove(new_pkg);
    force_remove(renamed);
    force_remove(pkg);
}

static void test_rename_many_files(void)
{
    char dir[256], renamed[256], path[256], content[64];

    snprintf(dir, sizeof(dir), "%s/rnm_pkg", BASE);
    snprintf(renamed, sizeof(renamed), "%s/rnm_~pkg", BASE);

    CHECK(mkdir(dir, 0755) == 0, "mkdir pkg");

    /* create enough files to exercise multi-entry directory handling */
    int ok = 1;
    for (int i = 0; i < RENAME_MANY_FILE_COUNT; i++) {
        snprintf(path, sizeof(path), "%s/file_%02d.py", dir, i);
        snprintf(content, sizeof(content), "# file %d\n", i);
        if (write_file(path, content) < 0)
            ok = 0;
    }
    CHECK(ok, "created files for rename-many test");

    CHECK_RET(rename(dir, renamed), 0, "rename pkg -> ~pkg (many files)");

    /* readdir must see all files */
    CHECK(count_dir_entries(renamed) == RENAME_MANY_FILE_COUNT,
          "readdir ~pkg sees all renamed files");

    /* delete all files, then rmdir */
    DIR *d = opendir(renamed);
    if (d) {
        struct dirent *ent;
        while ((ent = readdir(d)) != NULL) {
            if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0)
                continue;
            snprintf(path, sizeof(path), "%s/%s", renamed, ent->d_name);
            unlink(path);
        }
        closedir(d);
    }

    CHECK_RET(rmdir(renamed), 0, "rmdir ~pkg after unlinking all files");
    CHECK(access(renamed, F_OK) != 0, "~pkg fully removed");

    force_remove(renamed);
    force_remove(dir);
}

/* ========== readdir offset after delete ========== */

/*
 * Reproduce the rm -rf bug: read entries with getdents64 using a tiny buffer
 * (1 entry per call), delete each entry immediately, then continue reading.
 * On correct kernel (byte offsets), the fd position advances past the deleted
 * entry and lands on the next one. On buggy kernel (logical indices), the
 * index shifts after deletion and entries get skipped.
 */
static void test_readdir_offset_after_delete(void)
{
    char dir[256], path[256], content[64];

    snprintf(dir, sizeof(dir), "%s/readdir_del", BASE);
    CHECK(mkdir(dir, 0755) == 0, "mkdir readdir_del");

    /* Create enough files to force multiple getdents calls with a tiny buffer. */
    int ok = 1;
    for (int i = 0; i < READDIR_DELETE_FILE_COUNT; i++) {
        snprintf(path, sizeof(path), "%s/f%02d", dir, i);
        snprintf(content, sizeof(content), "%d\n", i);
        if (write_file(path, content) < 0)
            ok = 0;
    }
    CHECK(ok, "created files for read-delete loop");

    /*
     * Read-delete loop: tiny buffer forces 1 entry per getdents64 call.
     * After reading each entry, delete it immediately. If offset tracking
     * is correct, we'll see all entries. If buggy, entries get skipped.
     */
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);
    CHECK(dfd >= 0, "open dir for read-delete loop");

    char buf[32]; /* minimum buffer: exactly 1 entry */
    int total_deleted = 0;
    int rounds = 0;

    for (;;) {
        int nread = syscall(SYS_getdents64, dfd, buf, sizeof(buf));
        if (nread <= 0)
            break;

        int pos = 0;
        while (pos < nread) {
            struct linux_dirent64 *ent =
                (struct linux_dirent64 *)(buf + pos);
            if (strcmp(ent->d_name, ".") != 0 &&
                strcmp(ent->d_name, "..") != 0) {
                snprintf(path, sizeof(path), "%s/%s", dir, ent->d_name);
                if (unlink(path) == 0)
                    total_deleted++;
            }
            pos += ent->d_reclen;
        }
        rounds++;
    }
    close(dfd);

    CHECK(total_deleted == READDIR_DELETE_FILE_COUNT,
          "read-delete loop: all files deleted");
    CHECK(rounds > 1,
          "read-delete loop: needed multiple getdents calls (bug trigger)");

    /* If bug exists, some files were skipped and rmdir fails */
    int rmdir_ret = rmdir(dir);
    CHECK_RET(rmdir_ret, 0,
              "rmdir after read-delete loop succeeds (no skipped entries)");
    if (rmdir_ret != 0)
        force_remove(dir);
}

/*
 * Variant: read-readdir-delete-readdir pattern (simulates rm -rf).
 * Read all entries via getdents64, delete each batch before reading next.
 */
static void test_rm_rf_pattern(void)
{
    char dir[256], path[256], content[64];

    snprintf(dir, sizeof(dir), "%s/rmrf", BASE);
    CHECK(mkdir(dir, 0755) == 0, "mkdir rmrf");

    /* Create enough files to force multiple batched getdents calls. */
    for (int i = 0; i < RM_RF_FILE_COUNT; i++) {
        snprintf(path, sizeof(path), "%s/f%02d", dir, i);
        snprintf(content, sizeof(content), "%d\n", i);
        write_file(path, content);
    }

    /*
     * Simulate rm -rf: open dir, read small batch, delete those files,
     * repeat until empty. If offset is logical and entries are deleted
     * between getdents calls, files get skipped and rmdir fails.
     */
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);
    CHECK(dfd >= 0, "open dir for rm-rf pattern");

    char buf[80];
    int total_deleted = 0;
    int rounds = 0;

    for (;;) {
        /* Read one batch */
        int nread = syscall(SYS_getdents64, dfd, buf, sizeof(buf));
        if (nread <= 0)
            break;

        /* Collect names from this batch */
        char names[4][32];
        int name_count = 0;
        int pos = 0;
        while (pos < nread && name_count < 4) {
            struct linux_dirent64 *ent =
                (struct linux_dirent64 *)(buf + pos);
            if (strcmp(ent->d_name, ".") != 0 &&
                strcmp(ent->d_name, "..") != 0) {
                strncpy(names[name_count], ent->d_name, 31);
                names[name_count][31] = '\0';
                name_count++;
            }
            pos += ent->d_reclen;
        }

        /* Delete this batch */
        for (int i = 0; i < name_count; i++) {
            snprintf(path, sizeof(path), "%s/%s", dir, names[i]);
            if (unlink(path) == 0)
                total_deleted++;
        }
        rounds++;
    }
    close(dfd);

    CHECK(total_deleted == RM_RF_FILE_COUNT,
          "rm-rf pattern: all files deleted");
    CHECK(rounds > 1,
          "rm-rf pattern: needed multiple getdents calls");

    /*
     * If the bug exists, some files were skipped by getdents and remain.
     * rmdir should succeed only if all files were deleted.
     */
    int rmdir_ret = rmdir(dir);
    CHECK_RET(rmdir_ret, 0,
              "rmdir after rm-rf pattern succeeds (no leftover files)");
    if (rmdir_ret != 0)
        force_remove(dir);
}

/* ========== main ========== */

int main(void)
{
    TEST_START("bug-ext4-dir-ops: rmdir + rename + stale parent");

    manual_rmdir_recursive(BASE);
    CHECK(mkdir(BASE, 0755) == 0, "mkdir base");

    char parent[256], sibling[256], empty_dir[256];
    snprintf(parent, sizeof(parent), "%s/parent", BASE);
    snprintf(sibling, sizeof(sibling), "%s/parent/sibling.txt", BASE);
    snprintf(empty_dir, sizeof(empty_dir), "%s/parent/empty", BASE);
    CHECK(mkdir(parent, 0755) == 0, "mkdir parent");
    CHECK(write_file(sibling, "sibling\n") == 0, "create sibling.txt");
    CHECK(mkdir(empty_dir, 0755) == 0, "mkdir empty (control)");

    /* rmdir tests */
    test_rmdir_nonempty_mixed();
    test_rmdir_nonempty_files();
    test_rmdir_nonempty_subdirs();
    test_rmdir_nonempty_parent();
    test_rmdir_empty();

    /* rename basic semantics */
    test_rename_dir_to_nonempty_dir();
    test_rename_dir_to_empty_dir();
    test_rename_file_to_dir();
    test_rename_dir_to_file();
    test_rename_file_to_file();

    /* rename + VFS dentry cache (stale parent) */
    test_rename_then_unlink();
    test_rename_file_then_delete_old_path();
    test_rename_dir_then_delete_child_old_path();
    test_rename_then_readdir();
    test_rename_create_same_name();
    test_pip_upgrade_pattern();
    test_rename_many_files();

    /* readdir offset correctness after deletion (rm -rf bug) */
    test_readdir_offset_after_delete();
    test_rm_rf_pattern();

    manual_rmdir_recursive(BASE);

    TEST_DONE();
}
